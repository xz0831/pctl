//! Change lens: what changed in a watched file since the user last looked.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Write},
    ops::Range,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The names this module watches (user decision 2026-10-12).
const WATCHED: [&str; 4] = ["STATE.md", "AGENTS.md", "AGENTS.global.md", "CLAUDE.md"];
/// Above this many lines on either side the diff degrades to the whole text.
const MAX_LINES: usize = 5000;
/// Snapshots larger than this are not stored.
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

pub fn is_watched(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| WATCHED.contains(&name))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

/// Line-based LCS diff; adjacent edits are one Change.
/// Pure insert: `old` is empty. Pure delete: `new` is empty.
/// Lines are counted the way a viewer draws them: a trailing newline starts no line.
pub fn diff_lines(old: &str, new: &str) -> Vec<Change> {
    let (a, b) = (lines(old), lines(new));
    if a.len() > MAX_LINES || b.len() > MAX_LINES {
        return vec![Change {
            old: 0..a.len(),
            new: 0..b.len(),
        }];
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bj) in matches(&a, &b).into_iter().chain([(a.len(), b.len())]) {
        if ai > i || bj > j {
            out.push(Change {
                old: i..ai,
                new: j..bj,
            });
        }
        i = ai + 1;
        j = bj + 1;
    }
    out
}

/// None when the path is not watched or there is no snapshot yet.
/// `Some(vec![])` when the file reads the same as the snapshot.
pub fn changes(store: &Path, path: &Path, current: &str) -> Option<Vec<Change>> {
    if !is_watched(path) {
        return None;
    }
    let seen = fs::read_to_string(snapshot_path(store, path)?).ok()?;
    Some(diff_lines(&seen, current))
}

/// Save `current` as the snapshot. Silent Ok when the path is not watched
/// or the text is larger than 1 MiB; IO errors are real errors.
pub fn mark_seen(store: &Path, path: &Path, current: &str) -> io::Result<()> {
    if !is_watched(path) || current.len() > MAX_SNAPSHOT_BYTES {
        return Ok(());
    }
    let Some(file) = snapshot_path(store, path) else {
        return Ok(());
    };
    // An existing folder keeps whatever mode it already has.
    let created = !store.is_dir();
    fs::create_dir_all(store)?;
    if created {
        fs::set_permissions(store, fs::Permissions::from_mode(0o700))?;
    }
    write_atomic(&file, current.as_bytes())
}

/// $HOME/.local/state/mdv/seen
pub fn default_store() -> Option<PathBuf> {
    Some(PathBuf::from(env::var_os("HOME")?).join(".local/state/mdv/seen"))
}

/// One snapshot per absolute path, named by the hash of that path.
fn snapshot_path(store: &Path, path: &Path) -> Option<PathBuf> {
    let absolute = fs::canonicalize(path)
        .or_else(|_| env::current_dir().map(|cwd| cwd.join(path)))
        .ok()?;
    let name = format!(
        "{:x}.md",
        Sha256::digest(absolute.to_string_lossy().as_bytes())
    );
    Some(store.join(name))
}

/// Temp file in the same folder, then rename, so a reader never sees half a snapshot.
fn write_atomic(file: &Path, data: &[u8]) -> io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(".{name}.{:016x}.tmp", nonce()));
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let written = handle.write_all(data).and_then(|()| handle.sync_all());
    drop(handle);
    written
        .and_then(|()| fs::rename(&temp, file))
        .inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
}

fn nonce() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("random");
    u64::from_be_bytes(bytes)
}

/// Physical lines; no phantom last line for a trailing newline.
fn lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Indices of one longest common subsequence, increasing on both sides.
fn matches(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    lcs(a, b, 0, 0, &mut out);
    out
}

/// Hirschberg: O(n*m) time but linear space, so two 5000-line files stay cheap.
fn lcs(a: &[&str], b: &[&str], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    if a.is_empty() || b.is_empty() {
        return;
    }
    if a.len() == 1 {
        if let Some(at) = b.iter().position(|line| *line == a[0]) {
            out.push((ao, bo + at));
        }
        return;
    }
    let mid = a.len() / 2;
    let (left, right) = (&a[..mid], &a[mid..]);
    let forward = lcs_lengths(left, b);
    // LCS(right, b[k..]) is the reversed LCS of both tails' prefixes, so one pass
    // of lcs_lengths answers every suffix.
    let tail: Vec<&str> = right.iter().rev().copied().collect();
    let reversed: Vec<&str> = b.iter().rev().copied().collect();
    let mut backward = lcs_lengths(&tail, &reversed);
    backward.reverse();
    let split = (0..=b.len())
        .max_by_key(|k| forward[*k] + backward[*k])
        .unwrap_or(0);
    lcs(left, &b[..split], ao, bo, out);
    lcs(right, &b[split..], ao + mid, bo + split, out);
}

/// LCS length of `a` against every prefix of `b`, one entry per prefix.
fn lcs_lengths(a: &[&str], b: &[&str]) -> Vec<usize> {
    let mut prev = vec![0; b.len() + 1];
    let mut cur = vec![0; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("mdv-lens-{tag}-{:016x}", nonce()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scratch(tag: &str) -> (PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let store = dir.join("seen");
        (dir, store)
    }

    #[test]
    fn watches_only_the_agreed_names() {
        assert!(is_watched(Path::new("STATE.md")));
        assert!(is_watched(Path::new("/a/b/AGENTS.md")));
        assert!(is_watched(Path::new("/a/b/AGENTS.global.md")));
        assert!(is_watched(Path::new("CLAUDE.md")));
        assert!(!is_watched(Path::new("notes.md")));
        assert!(!is_watched(Path::new("STATE.md.bak")));
        assert!(!is_watched(Path::new("")));
    }

    #[test]
    fn no_snapshot_then_unchanged_text() {
        let (dir, store) = scratch("first");
        let file = dir.join("STATE.md");
        let text = "a\nb\nc\n";
        fs::write(&file, text).unwrap();
        assert_eq!(changes(&store, &file, text), None);
        mark_seen(&store, &file, text).unwrap();
        assert_eq!(changes(&store, &file, text), Some(vec![]));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inserted_line_has_empty_old_range() {
        assert_eq!(
            diff_lines("a\nb\n", "a\nx\nb\n"),
            vec![Change {
                old: 1..1,
                new: 1..2
            }]
        );
    }

    #[test]
    fn deleted_line_has_empty_new_range() {
        assert_eq!(
            diff_lines("a\nb\nc\n", "a\nc\n"),
            vec![Change {
                old: 1..2,
                new: 1..1
            }]
        );
    }

    #[test]
    fn two_adjacent_changed_lines_are_one_change() {
        assert_eq!(
            diff_lines("a\nb\nc\nd\n", "a\nX\nY\nd\n"),
            vec![Change {
                old: 1..3,
                new: 1..3
            }]
        );
    }

    #[test]
    fn two_separate_edits_stay_in_order() {
        assert_eq!(
            diff_lines("a\nb\nc\nd\ne\n", "a\nX\nc\nd\nY\n"),
            vec![
                Change {
                    old: 1..2,
                    new: 1..2
                },
                Change {
                    old: 4..5,
                    new: 4..5
                }
            ]
        );
    }

    #[test]
    fn applying_changes_rebuilds_the_new_text() {
        let cases = [
            ("", "a\nb\n"),
            ("a\nb\n", ""),
            ("a\nb\nc\n", "a\nc\nb\n"),
            ("x\ny\nz\n", "x\nz\n"),
            ("one\ntwo\nthree\nfour\n", "one\nTWO\nthree\nFOUR\nfive\n"),
            ("a\na\na\n", "a\na\n"),
            ("same\ntext\n", "same\ntext\n"),
        ];
        for (old, new) in cases {
            let (mut after, expected) = (lines(old), lines(new));
            for change in diff_lines(old, new).iter().rev() {
                after.splice(
                    change.old.clone(),
                    expected[change.new.clone()].iter().copied(),
                );
            }
            assert_eq!(after, expected, "old {old:?} new {new:?}");
        }
    }

    #[test]
    fn huge_sides_collapse_to_one_change() {
        let old = "x\n".repeat(MAX_LINES + 1);
        let new = "y\n".repeat(MAX_LINES + 2);
        assert_eq!(
            diff_lines(&old, &new),
            vec![Change {
                old: 0..MAX_LINES + 1,
                new: 0..MAX_LINES + 2
            }]
        );
    }

    #[test]
    fn unwatched_path_writes_nothing_and_reads_none() {
        let (dir, store) = scratch("unwatched");
        let file = dir.join("notes.md");
        fs::write(&file, "a\n").unwrap();
        mark_seen(&store, &file, "a\n").unwrap();
        assert!(!store.exists());
        assert_eq!(changes(&store, &file, "a\n"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_and_folder_are_private() {
        let (dir, store) = scratch("mode");
        let file = dir.join("STATE.md");
        fs::write(&file, "a\n").unwrap();
        mark_seen(&store, &file, "a\n").unwrap();
        let snapshot = snapshot_path(&store, &file).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&snapshot), 0o600);
        assert_eq!(mode(&store), 0o700);
        fs::remove_dir_all(&dir).unwrap();
    }
}
