//! Test-only helpers shared by the PDF pipeline's unit tests.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use std::os::unix::fs::PermissionsExt;

/// `execve` fails with ETXTBSY (os error 26) while any process still holds the
/// file open for writing. Closing our own fd is not enough: a sibling test
/// thread may `fork()` between `create` and `close`, and the child keeps a copy
/// of the write fd until it reaches its own `exec`.
const ETXTBSY: i32 = 26;
const EXEC_READY_ATTEMPTS: u32 = 200;
const EXEC_READY_POLL: Duration = Duration::from_millis(5);
/// Set only for the readiness probe; the header exits before the script body
/// runs, so probing has no side effects.
const PROBE_ENV: &str = "HANDOFF_TEST_EXEC_PROBE";

/// Writes `path` as an executable shell script that is safe to run immediately.
///
/// `body` is everything after the shebang. Returns only once the kernel no
/// longer reports the file as busy, so the invocation under test cannot fail
/// with ETXTBSY under parallel `cargo test`.
pub(crate) fn write_executable_script(path: &Path, body: &str) {
    {
        let mut f = std::fs::File::create(path).unwrap();
        let content = format!("#!/bin/sh\n[ -n \"${PROBE_ENV}\" ] && exit 0\n{body}");
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..EXEC_READY_ATTEMPTS {
        match std::process::Command::new(path)
            .env(PROBE_ENV, "1")
            .output()
        {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => std::thread::sleep(EXEC_READY_POLL),
            _ => return,
        }
    }
    panic!("{} still busy (ETXTBSY) after waiting", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_script_runs_its_body_and_the_probe_header_is_inert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        write_executable_script(&path, "echo ran\n");

        let out = std::process::Command::new(&path).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
    }

    #[test]
    fn scripts_are_runnable_at_once_while_sibling_threads_fork() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let forkers: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = std::process::Command::new("true").status();
                    }
                })
            })
            .collect();

        let dir = tempfile::tempdir().unwrap();
        for i in 0..100 {
            let path = dir.path().join(format!("tool{i}"));
            write_executable_script(&path, "echo ok\n");
            let out = std::process::Command::new(&path)
                .output()
                .unwrap_or_else(|e| panic!("run {i}: {e}"));
            assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for f in forkers {
            f.join().unwrap();
        }
    }
}
