//! Reads the list of files Git tracks straight from the repository's index
//! file, without starting `git`.
//!
//! The readiness and release checks must answer "is a secrets file tracked?"
//! from inside the MCP server, which never starts a process (SPEC-ONB-040,
//! SPEC-ONB-050). The other Git helpers in `storage::git` ask the `git`
//! binary; this module only reads files: the `.git` entry, `config`,
//! `commondir` and `index`.
//!
//! It understands index versions 2, 3 and 4 and both object formats (SHA-1,
//! SHA-256). An index whose entry list is not the whole story is reported as
//! [`TrackedFiles::Unreadable`] rather than read in part: a split index keeps
//! most entries in a second file, and a sparse index stands in whole
//! directories with one entry. Answering from such a list could miss a tracked
//! file, and a check built on this must never say "nothing is tracked" when
//! it could not see everything.

use std::path::{Path, PathBuf};

/// Signature every index file starts with.
const INDEX_SIGNATURE: &[u8; 4] = b"DIRC";
/// Bytes of the index header: signature, version, entry count.
const HEADER_LEN: usize = 12;
/// Bytes of an entry before the object id: ctime, mtime, dev, ino, mode, uid,
/// gid and size.
const ENTRY_STAT_LEN: usize = 40;
/// Offset of the mode field inside an entry.
const ENTRY_MODE_OFFSET: usize = 24;
/// Entry flag: a second 16-bit flags field follows (version 3 and later).
const FLAG_EXTENDED: u16 = 0x4000;
/// Entry flags: the path length, or this value when the path is longer.
const FLAG_NAME_MASK: u16 = 0x0fff;
/// Mask of the file-type bits of an entry's mode, and the type of a directory
/// (which only a sparse index stores).
const MODE_TYPE_MASK: u32 = 0o170_000;
const MODE_DIRECTORY: u32 = 0o040_000;
/// Extension holding the link to the shared index of a split index.
const EXT_SPLIT_INDEX: &[u8; 4] = b"link";
/// Extension marking a sparse index.
const EXT_SPARSE_INDEX: &[u8; 4] = b"sdir";
/// Object id lengths, in bytes.
const SHA1_LEN: usize = 20;
const SHA256_LEN: usize = 32;

/// The files Git tracks under a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackedFiles {
    /// The directory is not inside a Git working tree.
    NotARepository,
    /// It is, but the index could not be read completely. The text says why.
    Unreadable(String),
    /// Paths of the tracked files under the directory, relative to it, with
    /// `/` separators, sorted, each listed once.
    Files(Vec<String>),
}

/// The files Git tracks under `project_dir`. Reads files only; never starts a
/// process and never writes.
///
/// The repository is the nearest one containing `project_dir` (the directory
/// itself or an ancestor, as `git` resolves it). Only files under
/// `project_dir` are listed, so a project kept in a subdirectory of a larger
/// repository answers for its own files.
pub fn tracked_files(project_dir: &Path) -> TrackedFiles {
    let Some(repository) = find_repository(project_dir) else {
        return TrackedFiles::NotARepository;
    };
    let git_dir = match repository.git_dir {
        Ok(git_dir) => git_dir,
        Err(reason) => return TrackedFiles::Unreadable(reason),
    };
    let index_path = git_dir.join("index");
    let bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        // A repository nothing was ever added to has no index file.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return TrackedFiles::Files(Vec::new())
        }
        Err(e) => {
            return TrackedFiles::Unreadable(format!(
                "cannot read the Git index {} ({e})",
                index_path.display()
            ))
        }
    };
    let paths = match parse_index(&bytes, object_id_len(&git_dir)) {
        Ok(paths) => paths,
        Err(reason) => return TrackedFiles::Unreadable(reason),
    };

    let prefix = repository.prefix;
    let mut files: Vec<String> = paths
        .iter()
        .filter_map(|path| path.strip_prefix(prefix.as_slice()))
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    files.sort();
    files.dedup();
    TrackedFiles::Files(files)
}

struct Repository {
    /// The directory holding `index`, or why it could not be determined.
    git_dir: Result<PathBuf, String>,
    /// `project_dir` relative to the working tree root, as the index spells
    /// it: empty at the root, otherwise `a/b/`.
    prefix: Vec<u8>,
}

/// The nearest repository containing `project_dir`.
fn find_repository(project_dir: &Path) -> Option<Repository> {
    for root in project_dir.ancestors() {
        let dot_git = root.join(".git");
        let git_dir = if dot_git.is_dir() {
            Ok(dot_git)
        } else if dot_git.is_file() {
            linked_git_dir(root, &dot_git)
        } else {
            continue;
        };
        let relative = project_dir.strip_prefix(root).ok()?;
        let mut prefix = Vec::new();
        for component in relative.components() {
            prefix.extend_from_slice(component.as_os_str().to_string_lossy().as_bytes());
            prefix.push(b'/');
        }
        return Some(Repository { git_dir, prefix });
    }
    None
}

/// The Git directory a `.git` file points at (a linked worktree or a
/// submodule): `gitdir: <path>`, relative to the directory holding the file.
fn linked_git_dir(root: &Path, dot_git: &Path) -> Result<PathBuf, String> {
    let unusable = || {
        format!(
            "{} does not name a Git directory that can be read",
            dot_git.display()
        )
    };
    let pointer = std::fs::read_to_string(dot_git).map_err(|_| unusable())?;
    let target = pointer
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .ok_or_else(unusable)?;
    let git_dir = root.join(target);
    if git_dir.join("HEAD").is_file() {
        Ok(git_dir)
    } else {
        Err(unusable())
    }
}

/// Length of an object id in this repository's index: 32 bytes when the
/// repository uses SHA-256 (`extensions.objectformat`), otherwise 20.
fn object_id_len(git_dir: &Path) -> usize {
    // A linked worktree keeps its config in the shared directory.
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|common| git_dir.join(common.trim()));
    let uses_sha256 = [Some(git_dir.to_path_buf()), common_dir]
        .into_iter()
        .flatten()
        .filter_map(|dir| std::fs::read_to_string(dir.join("config")).ok())
        .any(|config| {
            config.lines().any(|line| {
                let line = line.trim().to_ascii_lowercase();
                line.strip_prefix("objectformat")
                    .and_then(|rest| rest.trim_start().strip_prefix('='))
                    .is_some_and(|value| value.trim() == "sha256")
            })
        });
    if uses_sha256 {
        SHA256_LEN
    } else {
        SHA1_LEN
    }
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(field.try_into().ok()?))
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let field = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes(field.try_into().ok()?))
}

/// Position of the NUL that ends the string starting at `from`.
fn find_nul(bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|byte| *byte == 0)
        .map(|offset| from + offset)
}

/// Reads the variable-width integer index version 4 uses for "how many bytes
/// of the previous path to drop". Returns the value and the bytes consumed.
fn read_varint(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut at = from;
    let mut byte = *bytes.get(at)?;
    at += 1;
    let mut value = usize::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        byte = *bytes.get(at)?;
        at += 1;
        value = value
            .checked_add(1)?
            .checked_mul(128)?
            .checked_add(usize::from(byte & 0x7f))?;
    }
    Some((value, at - from))
}

/// The path of every entry in an index file, or why the file cannot be
/// trusted to list every tracked file.
fn parse_index(bytes: &[u8], object_id_len: usize) -> Result<Vec<Vec<u8>>, String> {
    let truncated = || "the Git index is truncated or corrupt".to_string();

    if bytes.get(..INDEX_SIGNATURE.len()) != Some(INDEX_SIGNATURE.as_slice()) {
        return Err("the Git index does not start with the index signature".to_string());
    }
    let version = read_u32(bytes, 4).ok_or_else(truncated)?;
    if !(2..=4).contains(&version) {
        return Err(format!(
            "Git index version {version} is not supported (versions 2 to 4 are)"
        ));
    }
    let count = read_u32(bytes, 8).ok_or_else(truncated)? as usize;
    // The file ends with a checksum over everything before it.
    let content_end = bytes
        .len()
        .checked_sub(object_id_len)
        .filter(|end| *end >= HEADER_LEN)
        .ok_or_else(truncated)?;
    let content = &bytes[..content_end];

    let fixed_len = ENTRY_STAT_LEN + object_id_len + 2;
    // An entry is at least its fixed part and a terminator, so a count the
    // file cannot hold is rejected before anything is allocated for it.
    if count > content.len() / fixed_len {
        return Err(truncated());
    }
    let mut paths: Vec<Vec<u8>> = Vec::with_capacity(count);
    let mut at = HEADER_LEN;
    let mut previous: Vec<u8> = Vec::new();
    let mut nameless_entry = false;
    for _ in 0..count {
        let start = at;
        let mode = read_u32(content, start + ENTRY_MODE_OFFSET).ok_or_else(truncated)?;
        let flags =
            read_u16(content, start + ENTRY_STAT_LEN + object_id_len).ok_or_else(truncated)?;
        at = start + fixed_len;
        if flags & FLAG_EXTENDED != 0 {
            if version < 3 {
                return Err(truncated());
            }
            at += 2;
        }
        if mode & MODE_TYPE_MASK == MODE_DIRECTORY {
            return Err(
                "the repository uses a sparse index, which lists whole directories as one \
                 entry; the tracked files cannot be listed from it"
                    .to_string(),
            );
        }

        let path = if version == 4 {
            let (dropped, used) = read_varint(content, at).ok_or_else(truncated)?;
            at += used;
            let end = find_nul(content, at).ok_or_else(truncated)?;
            let kept = previous.len().checked_sub(dropped).ok_or_else(truncated)?;
            let mut path = previous[..kept].to_vec();
            path.extend_from_slice(&content[at..end]);
            at = end + 1;
            path
        } else {
            let declared = usize::from(flags & FLAG_NAME_MASK);
            let end = if declared < usize::from(FLAG_NAME_MASK) {
                at.checked_add(declared).ok_or_else(truncated)?
            } else {
                find_nul(content, at).ok_or_else(truncated)?
            };
            if content.get(end) != Some(&0) {
                return Err(truncated());
            }
            let path = content[at..end].to_vec();
            // Entries are padded with 1 to 8 NULs to a multiple of 8 bytes.
            at = start + ((end - start + 8) & !7);
            path
        };
        if at > content.len() {
            return Err(truncated());
        }
        previous.clone_from(&path);
        if path.is_empty() {
            // Only a split index has entries without a path (they replace
            // entries of the shared index). Which case this is shows in the
            // extensions below.
            nameless_entry = true;
        } else {
            paths.push(path);
        }
    }

    // Extensions: a 4-byte name, a 4-byte length, then that many bytes.
    while at < content.len() {
        let name = content.get(at..at + 4).ok_or_else(truncated)?;
        let len = read_u32(content, at + 4).ok_or_else(truncated)? as usize;
        let data_start = at + 8;
        let data_end = data_start.checked_add(len).ok_or_else(truncated)?;
        let data = content.get(data_start..data_end).ok_or_else(truncated)?;
        if name == EXT_SPLIT_INDEX.as_slice() {
            let shared = data.get(..object_id_len).ok_or_else(truncated)?;
            if shared.iter().any(|byte| *byte != 0) {
                return Err(
                    "the repository uses a split index, which keeps most entries in a second \
                     file; the tracked files cannot be listed from it"
                        .to_string(),
                );
            }
        }
        if name == EXT_SPARSE_INDEX.as_slice() {
            return Err(
                "the repository uses a sparse index; the tracked files cannot be listed from it"
                    .to_string(),
            );
        }
        at = data_end;
    }
    if nameless_entry {
        return Err(truncated());
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Runs `git` in `dir` for a test fixture. Only the tests start `git`;
    /// the code under test reads the files it leaves behind.
    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .expect("git must be installed to run these tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn repo(init_args: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut args = vec!["init", "-q"];
        args.extend_from_slice(init_args);
        git(dir.path(), &args);
        dir
    }

    fn files(dir: &Path) -> Vec<String> {
        match tracked_files(dir) {
            TrackedFiles::Files(files) => files,
            other => panic!("expected a file list, got {other:?}"),
        }
    }

    fn unreadable(dir: &Path) -> String {
        match tracked_files(dir) {
            TrackedFiles::Unreadable(reason) => reason,
            other => panic!("expected an unreadable index, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_outside_git_is_not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".env", "x");
        assert_eq!(tracked_files(dir.path()), TrackedFiles::NotARepository);
    }

    #[test]
    fn a_repository_nothing_was_added_to_tracks_nothing() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn tracked_files_are_listed_and_untracked_ones_are_not() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        write(dir.path(), "src/deep/main.rs", "x");
        write(dir.path(), "README.md", "x");
        write(dir.path(), "untracked.pem", "x");
        git(dir.path(), &["add", ".env", "src", "README.md"]);
        assert_eq!(
            files(dir.path()),
            vec![".env", "README.md", "src/deep/main.rs"]
        );
        // Committing changes nothing about what is tracked.
        git(dir.path(), &["commit", "-q", "-m", "x"]);
        assert_eq!(
            files(dir.path()),
            vec![".env", "README.md", "src/deep/main.rs"]
        );
    }

    #[test]
    fn a_file_removed_from_the_index_is_no_longer_listed() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        write(dir.path(), "a.txt", "x");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["rm", "-q", "--cached", ".env"]);
        assert_eq!(files(dir.path()), vec!["a.txt"]);
        assert!(dir.path().join(".env").exists());
    }

    #[test]
    fn many_files_with_shared_prefixes_survive_index_version_4() {
        let dir = repo(&[]);
        let mut expected = Vec::new();
        for i in 0..40 {
            let name = format!("pkg/module_{i:02}/src/file_{i:02}.rs");
            write(dir.path(), &name, "x");
            expected.push(name);
        }
        write(dir.path(), "pkg/.env", "x");
        expected.push("pkg/.env".to_string());
        write(dir.path(), "z", "x");
        expected.push("z".to_string());
        expected.sort();
        git(dir.path(), &["add", "."]);
        let version_2 = files(dir.path());
        assert_eq!(version_2, expected);

        git(dir.path(), &["update-index", "--index-version", "4"]);
        let bytes = std::fs::read(dir.path().join(".git/index")).unwrap();
        assert_eq!(read_u32(&bytes, 4), Some(4), "the fixture is a v4 index");
        assert_eq!(files(dir.path()), expected);
    }

    #[test]
    fn an_intent_to_add_entry_uses_extended_flags_and_is_listed() {
        let dir = repo(&[]);
        write(dir.path(), "a.txt", "x");
        write(dir.path(), "later.key", "x");
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["add", "-N", "later.key"]);
        let bytes = std::fs::read(dir.path().join(".git/index")).unwrap();
        assert_eq!(read_u32(&bytes, 4), Some(3), "the fixture is a v3 index");
        assert_eq!(files(dir.path()), vec!["a.txt", "later.key"]);
    }

    #[test]
    fn a_path_longer_than_the_length_field_is_read_to_its_terminator() {
        let dir = repo(&[]);
        write(dir.path(), "z.txt", "x");
        git(dir.path(), &["add", "."]);
        // 21 segments of 200 characters: longer than the 4095 the flags hold,
        // and longer than a file system lets a real file be, so the entry is
        // written into the index directly.
        let segment = "d".repeat(200);
        let long = format!("{}/f.txt", vec![segment.as_str(); 21].join("/"));
        assert!(long.len() > usize::from(FLAG_NAME_MASK));
        let empty_blob = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{empty_blob},{long}"),
            ],
        );
        write(dir.path(), "zz.txt", "x");
        git(dir.path(), &["add", "zz.txt"]);
        assert_eq!(
            files(dir.path()),
            vec![long, "z.txt".to_string(), "zz.txt".to_string()]
        );
    }

    #[test]
    fn names_outside_ascii_are_kept() {
        let dir = repo(&[]);
        write(dir.path(), "docs/秘密.txt", "x");
        write(dir.path(), "with space.env", "x");
        git(dir.path(), &["add", "."]);
        assert_eq!(files(dir.path()), vec!["docs/秘密.txt", "with space.env"]);
    }

    #[test]
    fn a_sha256_repository_is_read_with_its_longer_object_ids() {
        let dir = repo(&["--object-format=sha256"]);
        write(dir.path(), ".env", "x");
        write(dir.path(), "src/a.rs", "x");
        write(dir.path(), "src/b.rs", "x");
        git(dir.path(), &["add", "."]);
        assert_eq!(files(dir.path()), vec![".env", "src/a.rs", "src/b.rs"]);
        git(dir.path(), &["update-index", "--index-version", "4"]);
        assert_eq!(files(dir.path()), vec![".env", "src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn a_project_in_a_subdirectory_answers_for_its_own_files() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        write(dir.path(), "apps/web/.env.local", "x");
        write(dir.path(), "apps/web/src/index.ts", "x");
        write(dir.path(), "apps/webby/secrets.yml", "x");
        git(dir.path(), &["add", "."]);
        assert_eq!(
            files(&dir.path().join("apps/web")),
            vec![".env.local", "src/index.ts"]
        );
        assert_eq!(files(&dir.path().join("apps/webby")), vec!["secrets.yml"]);
        // A directory Git tracks nothing in.
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        assert_eq!(files(&dir.path().join("empty")), Vec::<String>::new());
    }

    #[test]
    fn a_linked_worktree_is_read_from_its_own_index() {
        let dir = repo(&[]);
        write(dir.path(), "a.txt", "x");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "x"]);
        let linked = dir.path().join("linked");
        git(
            dir.path(),
            &["worktree", "add", "-q", linked.to_str().unwrap()],
        );
        assert!(linked.join(".git").is_file());
        write(&linked, "only-here.pem", "x");
        git(&linked, &["add", "only-here.pem"]);
        assert_eq!(files(&linked), vec!["a.txt", "only-here.pem"]);
        assert_eq!(files(dir.path()), vec!["a.txt"]);
    }

    #[test]
    fn a_git_file_that_points_nowhere_is_unreadable_not_untracked() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".git",
            "gitdir: /nonexistent/.git/worktrees/x\n",
        );
        assert!(unreadable(dir.path()).contains(".git"));
        write(dir.path(), ".git", "not a pointer\n");
        assert!(unreadable(dir.path()).contains(".git"));
    }

    #[test]
    fn a_split_index_is_reported_instead_of_read_in_part() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        write(dir.path(), "a.txt", "x");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["update-index", "--split-index"]);
        write(dir.path(), "b.txt", "x");
        git(dir.path(), &["add", "b.txt"]);
        let reason = unreadable(dir.path());
        assert!(reason.contains("split index"), "{reason}");
    }

    #[test]
    fn a_sparse_index_is_reported_instead_of_read_in_part() {
        let dir = repo(&[]);
        write(dir.path(), "kept/a.txt", "x");
        write(dir.path(), "hidden/.env", "x");
        write(dir.path(), "top.txt", "x");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "x"]);
        git(
            dir.path(),
            &["sparse-checkout", "init", "--cone", "--sparse-index"],
        );
        git(dir.path(), &["sparse-checkout", "set", "kept"]);
        let reason = unreadable(dir.path());
        assert!(reason.contains("sparse index"), "{reason}");
    }

    #[test]
    fn a_truncated_or_corrupt_index_never_panics_and_never_lists_files() {
        let dir = repo(&[]);
        for i in 0..12 {
            write(dir.path(), &format!("dir/file_{i}.txt"), "x");
        }
        git(dir.path(), &["add", "."]);
        for version in ["2", "4"] {
            git(dir.path(), &["update-index", "--index-version", version]);
            let index = dir.path().join(".git/index");
            let whole = std::fs::read(&index).unwrap();
            assert!(parse_index(&whole, SHA1_LEN).is_ok());
            // Every shorter file, down to nothing.
            for len in 0..whole.len() {
                let _ = parse_index(&whole[..len], SHA1_LEN);
            }
            // A header that promises more entries than the file holds.
            let mut inflated = whole.clone();
            inflated[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(parse_index(&inflated, SHA1_LEN).is_err());
            // Flipping any single byte must not panic either.
            for at in 0..whole.len() {
                let mut flipped = whole.clone();
                flipped[at] ^= 0xff;
                let _ = parse_index(&flipped, SHA1_LEN);
            }
            // Cut inside the entries: reported, not answered with a short list.
            std::fs::write(&index, &whole[..whole.len() / 2]).unwrap();
            assert!(matches!(
                tracked_files(dir.path()),
                TrackedFiles::Unreadable(_)
            ));
            std::fs::write(&index, &whole).unwrap();
        }
        std::fs::write(dir.path().join(".git/index"), b"garbage").unwrap();
        assert!(unreadable(dir.path()).contains("signature"));
    }

    #[test]
    fn an_index_version_this_does_not_know_is_reported() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(INDEX_SIGNATURE);
        bytes.extend_from_slice(&5u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&[0; SHA1_LEN]);
        let reason = parse_index(&bytes, SHA1_LEN).unwrap_err();
        assert!(reason.contains("version 5"), "{reason}");
    }

    /// A file staged at several merge stages is one tracked file.
    #[test]
    fn a_conflicted_file_is_listed_once() {
        let dir = repo(&["-b", "main"]);
        write(dir.path(), "shared.key", "base\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        git(dir.path(), &["checkout", "-q", "-b", "other"]);
        write(dir.path(), "shared.key", "theirs\n");
        git(dir.path(), &["commit", "-q", "-am", "theirs"]);
        git(dir.path(), &["checkout", "-q", "main"]);
        write(dir.path(), "shared.key", "ours\n");
        git(dir.path(), &["commit", "-q", "-am", "ours"]);
        let merge = Command::new("git")
            .args(["merge", "-q", "other"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(!merge.status.success(), "the fixture needs a conflict");
        assert_eq!(files(dir.path()), vec!["shared.key"]);
    }

    #[test]
    fn reading_the_index_changes_nothing() {
        let dir = repo(&[]);
        write(dir.path(), ".env", "x");
        git(dir.path(), &["add", "."]);
        let index = dir.path().join(".git/index");
        let before = (
            std::fs::read(&index).unwrap(),
            std::fs::metadata(&index).unwrap().modified().unwrap(),
        );
        files(dir.path());
        let after = (
            std::fs::read(&index).unwrap(),
            std::fs::metadata(&index).unwrap().modified().unwrap(),
        );
        assert_eq!(before, after);
    }
}
