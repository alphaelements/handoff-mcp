//! Cross-platform behavior of the shared storage primitives.
//!
//! `expand_tilde` and `atomic_write` are the two places where `.handoff/` I/O
//! touched platform-specific assumptions (POSIX-only `HOME`, POSIX-only rename
//! semantics). These tests pin the portable contract on every host so a
//! regression shows up on Linux CI rather than only on a Windows user's machine.

use std::path::{Path, MAIN_SEPARATOR};

use handoff_mcp::storage::{atomic_write, expand_tilde};

/// Serializes the tests that mutate `HOME` / `USERPROFILE`, since env vars are
/// process-global and Rust runs tests in threads.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes [`ENV_LOCK`], recovering from poisoning so that one failing test does
/// not cascade into `PoisonError` failures in every other env-mutating test.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect::<Vec<_>>();
        for k in keys {
            std::env::remove_var(k);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[test]
fn expand_tilde_uses_home_when_set() {
    let _lock = env_lock();
    let _guard = EnvGuard::new(&["HOME", "USERPROFILE"]);

    std::env::set_var("HOME", "/home/alice");
    let got = expand_tilde("~/pro/handoff-mcp");

    assert_eq!(
        Path::new(&got),
        Path::new("/home/alice").join("pro").join("handoff-mcp"),
        "expand_tilde should resolve ~ against HOME"
    );
}

/// On Windows there is no `HOME`; the home directory lives in `USERPROFILE`.
/// `setup.rs` already falls back to it, so the storage layer must agree —
/// otherwise `~/`-prefixed `scan_dirs` silently stay unexpanded and
/// `handoff_dashboard` / `handoff_refer` skip every configured directory.
#[test]
fn expand_tilde_falls_back_to_userprofile() {
    let _lock = env_lock();
    let _guard = EnvGuard::new(&["HOME", "USERPROFILE"]);

    std::env::set_var("USERPROFILE", r"C:\Users\alice");
    let got = expand_tilde("~/pro/handoff-mcp");

    assert_ne!(
        got, "~/pro/handoff-mcp",
        "expand_tilde must not leave ~ unexpanded when USERPROFILE is set"
    );
    assert!(
        got.starts_with(r"C:\Users\alice"),
        "expected USERPROFILE-rooted path, got {got}"
    );
    assert!(
        got.ends_with("handoff-mcp"),
        "expected the tail to be preserved, got {got}"
    );
}

/// The joined separator must be the platform's, so the result round-trips
/// through `Path` and string comparisons in callers behave consistently.
#[test]
fn expand_tilde_uses_platform_separator() {
    let _lock = env_lock();
    let _guard = EnvGuard::new(&["HOME", "USERPROFILE"]);

    let home = if cfg!(windows) {
        r"C:\Users\alice"
    } else {
        "/home/alice"
    };
    std::env::set_var("HOME", home);

    let got = expand_tilde("~/pro");
    assert_eq!(
        got,
        format!("{home}{MAIN_SEPARATOR}pro"),
        "expand_tilde should join with the platform separator"
    );
}

#[test]
fn expand_tilde_leaves_unprefixed_paths_alone() {
    let _lock = env_lock();
    let _guard = EnvGuard::new(&["HOME", "USERPROFILE"]);
    std::env::set_var("HOME", "/home/alice");

    assert_eq!(expand_tilde("/abs/path"), "/abs/path");
    assert_eq!(expand_tilde("relative/path"), "relative/path");
    // A bare "~" has no trailing separator and is intentionally not expanded.
    assert_eq!(expand_tilde("~"), "~");
}

#[test]
fn expand_tilde_returns_input_when_no_home_vars() {
    let _lock = env_lock();
    let _guard = EnvGuard::new(&["HOME", "USERPROFILE"]);

    assert_eq!(expand_tilde("~/pro"), "~/pro");
}

#[test]
fn atomic_write_creates_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");

    atomic_write(&path, b"{\"a\":1}").unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
}

/// Every handoff write path overwrites an existing file (tasks, config,
/// sessions, timer state). This is the case that fails on Windows if the
/// implementation ever moves to a non-replacing rename.
#[test]
fn atomic_write_overwrites_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    atomic_write(&path, b"first").unwrap();
    atomic_write(&path, b"second").unwrap();
    atomic_write(&path, b"third").unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "third");
}

/// The temp file is an implementation detail — it must never survive a
/// successful write, or `.handoff/` accumulates `.tasks.json.tmp.<pid>` litter
/// that the VSCode extension would try to parse.
#[test]
fn atomic_write_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");

    atomic_write(&path, b"payload").unwrap();
    atomic_write(&path, b"payload2").unwrap();

    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "sessions.json")
        .collect::<Vec<_>>();

    assert!(
        leftovers.is_empty(),
        "expected no temp files left behind, found {leftovers:?}"
    );
}

/// A reader holding the file open must not break the writer. On Unix this is
/// free; on Windows an open handle is exactly what turns a replace into
/// ERROR_ACCESS_DENIED, which is why `atomic_write` retries.
#[test]
fn atomic_write_succeeds_while_file_is_open_for_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");

    atomic_write(&path, b"before").unwrap();

    let handle = std::fs::File::open(&path).unwrap();

    atomic_write(&path, b"after")
        .expect("atomic_write must succeed even while a reader holds the file open");

    drop(handle);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
}

/// Concurrent writers to the *same* destination must not corrupt each other.
/// Before temp names carried a per-call sequence number they were unique only
/// per process, so simultaneous writers picked the identical temp path: one
/// thread's `File::create` truncated the other's half-written temp file, and
/// whichever thread lost the race either renamed a spliced payload over the
/// destination or failed outright because its temp file had been renamed away.
///
/// Two independent assertions catch a regression, and both are load-bearing.
/// Detection rates below are measured by reverting the fix and re-running:
///
/// 1. **A failed write is the bug, not a flake** — caught in 10/10 runs at every
///    payload size tried. With unique temp names a writer has nothing to collide
///    with, so an error here *is* the regression: `PermissionDenied` is retried
///    internally by `replace_file`, and any other error means a rival writer
///    renamed this writer's temp file away. The message says exactly that, so a
///    future maintainer is not told a real corruption bug was "spurious".
/// 2. **The destination is sampled while the writers run** — a direct check on
///    the corruption itself. This must happen mid-run: checking only the
///    post-join file detects nothing, because the last write to complete is by
///    definition uncontended, so the final payload is always clean (0/20 runs
///    with the fix reverted). Sampling is size-sensitive (see `LEN`).
///
/// Each writer's payload is a single repeated byte, so any splice of two
/// writers' content fails the uniform-byte check even if the length happens to
/// come out right.
///
/// Thread count is deliberately low: the bug is pairwise, so four writers
/// reproduce it as reliably as many more would. Every writer's rename targets
/// one contended path, and on Windows each is retried for a bounded ~319ms
/// budget — piling on threads buys no detection power while risking a CI flake
/// whose signature is identical to a real regression.
#[test]
fn concurrent_writes_to_one_file_do_not_corrupt_each_other() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const THREADS: usize = 4;
    const ROUNDS: usize = 25;
    // 256KB, measured rather than guessed. Reverting the fix and running only
    // the sampler catches the corruption in 7/10 runs at 8KB and 5/10 at 64KB,
    // but 10/10 at 256KB: a smaller payload is copied so fast that the spliced
    // state usually closes between two samples. The writer assertion below is
    // 10/10 at every size, so the two together are belt and braces.
    const LEN: usize = 256 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");

    // Seed the file so the sampler always has a complete payload to read.
    atomic_write(&path, &vec![b'a'; LEN]).unwrap();

    let stop = AtomicBool::new(false);
    let samples = AtomicUsize::new(0);
    let torn = AtomicUsize::new(0);

    // Stops the sampler on the way out of the scope *however* we leave it. A
    // writer tripping its `expect` unwinds through here, so without this guard
    // the sampler would spin forever and `scope` would block joining it: the
    // regression would surface as a CI job that hangs until it is killed,
    // printing no assertion message at all.
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    std::thread::scope(|scope| {
        let _stopper = StopOnDrop(&stop);

        let sampler = scope.spawn(|| {
            // Also bounded, so a lost wake-up can never hang the suite.
            for _ in 0..50_000_000 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match std::fs::read(&path) {
                    Ok(c) => {
                        samples.fetch_add(1, Ordering::Relaxed);
                        // Uniform bytes and exact length: a payload spliced from
                        // two writers fails one or both.
                        if c.len() != LEN || !c.iter().all(|&b| b == c[0]) {
                            torn.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // Briefly absent mid-replace on some platforms; not a tear.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => panic!("unexpected read error: {e}"),
                }
            }
        });

        let writers = (0..THREADS)
            .map(|t| {
                let path = &path;
                scope.spawn(move || {
                    // b'a' + THREADS stays well inside u8 for any sane THREADS.
                    let body = vec![b'a' + t as u8; LEN];
                    for _ in 0..ROUNDS {
                        atomic_write(path, &body).expect(
                            "a writer's temp file was taken by another writer — \
                             temp names are no longer unique per call",
                        );
                    }
                })
            })
            .collect::<Vec<_>>();

        // Propagate a writer panic rather than swallowing it, but only after
        // every writer has been joined, so one failure does not mask the rest.
        let outcomes = writers.into_iter().map(|w| w.join()).collect::<Vec<_>>();
        stop.store(true, Ordering::Relaxed);
        sampler.join().unwrap();
        for o in outcomes {
            if let Err(panic) = o {
                std::panic::resume_unwind(panic);
            }
        }
    });

    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "sampler observed a spliced payload — concurrent writers corrupted each other"
    );
    // Without this the sampler could have read nothing and proved nothing.
    assert!(
        samples.load(Ordering::Relaxed) > 0,
        "sampler never read the file, so the atomicity claim is unverified"
    );

    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "tasks.json")
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "concurrent writes left temp files behind: {leftovers:?}"
    );
}

/// A reader must only ever observe one *complete* payload — never a torn,
/// truncated, or empty file — no matter how many writes land underneath it.
///
/// The reader keeps polling until it has actually witnessed the file change
/// (`saw_new`), then asserts that it did. Without that assertion the test is
/// vacuous: thread-spawn latency can let all reads complete before the first
/// write lands, so every read returns the original payload and the "no torn
/// read" claim is never exercised. The two payloads deliberately differ in
/// length as well as content, so a truncated read fails the length check even
/// if the byte pattern matches.
#[test]
fn atomic_write_never_exposes_partial_content() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");

    let old = "a".repeat(64 * 1024);
    let new = "b".repeat(96 * 1024);
    atomic_write(&path, old.as_bytes()).unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let path = path.clone();
        let new = new.clone();
        let old = old.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            // Keep rewriting until the reader confirms it saw the new payload,
            // so the race window stays open as long as the reader needs it.
            for _ in 0..10_000 {
                atomic_write(&path, new.as_bytes()).unwrap();
                if done.load(Ordering::Relaxed) {
                    break;
                }
                // Flip back so the reader can observe transitions repeatedly.
                atomic_write(&path, old.as_bytes()).unwrap();
            }
        })
    };

    let mut saw_new = false;
    let mut observations = 0usize;
    // Bounded so a pathologically slow machine cannot hang the suite.
    for _ in 0..200_000 {
        match std::fs::read(&path) {
            Ok(content) => {
                observations += 1;
                let matches_old = content.len() == old.len() && content == old.as_bytes();
                let matches_new = content.len() == new.len() && content == new.as_bytes();
                assert!(
                    matches_old || matches_new,
                    "observed a torn read of {} bytes (expected {} or {})",
                    content.len(),
                    old.len(),
                    new.len()
                );
                if matches_new {
                    saw_new = true;
                    break;
                }
            }
            // A reader may briefly find the path absent mid-replace on some
            // platforms; that is not a torn read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("unexpected read error: {e}"),
        }
    }

    done.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    assert!(
        saw_new,
        "reader never observed the concurrent write after {observations} reads — \
         the test would be vacuous, so the atomicity claim is unverified"
    );
}
