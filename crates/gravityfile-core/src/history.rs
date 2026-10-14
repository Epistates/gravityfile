//! Scan history: one summary line per full scan, appended to `history.jsonl`
//! in the data directory, so a place's growth can be followed over time.
//!
//! The file is JSON lines, read back leniently: a line that cannot be parsed,
//! perhaps written by a newer version, is skipped rather than failing the
//! whole history. It is trimmed to its newest half when it passes 4 MiB.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::FileTree;

/// The size at which the history is trimmed to its newest half.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// How many of the root's largest entries a record keeps.
pub const TOP_ENTRIES: usize = 20;

/// The summary of one scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When the scan finished.
    pub at: SystemTime,
    #[serde(with = "crate::path_serde")]
    pub root: PathBuf,
    /// Sizes are file lengths rather than disk usage.
    pub apparent: bool,
    pub size: u64,
    pub files: u64,
    pub directories: u64,
    /// The largest entries directly under the root, largest first.
    pub top: Vec<TopEntry>,
}

/// One of the largest entries under the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopEntry {
    #[serde(with = "crate::path_serde")]
    pub name: PathBuf,
    pub size: u64,
}

impl Record {
    /// The summary of `tree`.
    pub fn of(tree: &FileTree) -> Self {
        let mut top: Vec<TopEntry> = tree
            .root
            .children
            .iter()
            .map(|child| TopEntry {
                name: PathBuf::from(child.file_name()),
                size: child.size,
            })
            .collect();
        top.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        top.truncate(TOP_ENTRIES);
        Self {
            at: tree.scanned_at,
            root: tree.root_path.clone(),
            apparent: tree.config.apparent_size,
            size: tree.root.size,
            files: tree.stats.total_files,
            directories: tree.stats.total_dirs.saturating_sub(1),
            top,
        }
    }

    /// Whether the two records measured the same place the same way, so
    /// their sizes can be compared.
    pub fn comparable(&self, other: &Self) -> bool {
        self.root == other.root && self.apparent == other.apparent
    }

    /// The top entry whose size changed most since `earlier`, and by how
    /// much; entries missing on one side count as zero there.
    pub fn largest_change<'a>(&'a self, earlier: &'a Self) -> Option<(&'a Path, i128)> {
        let size_in = |record: &Self, name: &Path| {
            record
                .top
                .iter()
                .find(|entry| entry.name == name)
                .map_or(0, |entry| i128::from(entry.size))
        };
        self.top
            .iter()
            .map(|entry| entry.name.as_path())
            .chain(earlier.top.iter().map(|entry| entry.name.as_path()))
            .map(|name| (name, size_in(self, name) - size_in(earlier, name)))
            .filter(|(_, change)| *change != 0)
            .max_by_key(|(name, change)| (change.unsigned_abs(), std::cmp::Reverse(*name)))
    }
}

/// Append `record` to the history at `file`, creating it and its directory.
pub fn append(file: &Path, record: &Record) -> std::io::Result<()> {
    if let Some(directory) = file.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    line.push(b'\n');
    // One write per record keeps concurrent appends from interleaving.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(&line)?;
    if std::fs::metadata(file)?.len() > MAX_BYTES {
        trim(file)?;
    }
    Ok(())
}

/// Keep the newer half of the history.
fn trim(file: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(file)?;
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len() / 2..].join("\n") + "\n";
    let staged = file.with_extension("jsonl.tmp");
    std::fs::write(&staged, kept)?;
    std::fs::rename(staged, file)
}

/// Every record for `root`, oldest first, or for every root when `None`. A
/// missing history is empty.
pub fn read(file: &Path, root: Option<&Path>) -> std::io::Result<Vec<Record>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut records: Vec<Record> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter(|record| root.is_none_or(|root| record.root == root))
        .collect();
    // Appends from several processes can land slightly out of order.
    records.sort_by_key(|record| record.at);
    Ok(records)
}

/// The latest record comparable with `record` from before it, at least
/// `min_gap` earlier so a rescan moments later is not the baseline.
pub fn previous<'a>(
    records: &'a [Record],
    record: &Record,
    min_gap: Duration,
) -> Option<&'a Record> {
    records
        .iter()
        .rev()
        .filter(|earlier| earlier.comparable(record))
        .find(|earlier| {
            earlier.at < record.at
                && record
                    .at
                    .duration_since(earlier.at)
                    .is_ok_and(|gap| gap >= min_gap)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(root: &str, days: u64, size: u64, top: &[(&str, u64)]) -> Record {
        Record {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(86_400 * days),
            root: PathBuf::from(root),
            apparent: false,
            size,
            files: 1,
            directories: 0,
            top: top
                .iter()
                .map(|(name, size)| TopEntry {
                    name: PathBuf::from(name),
                    size: *size,
                })
                .collect(),
        }
    }

    #[test]
    fn records_append_and_read_back_per_root_in_time_order() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("data/history.jsonl");
        let later = record("/home", 9, 300, &[("a", 200)]);
        let earlier = record("/home", 2, 100, &[("a", 50)]);
        let other = record("/srv", 5, 7, &[]);
        for record in [&later, &other, &earlier] {
            append(&file, record).unwrap();
        }
        // A line from elsewhere is skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"{\"unexpected\":true}\nnot json\n")
            .unwrap();
        assert_eq!(
            read(&file, Some(Path::new("/home"))).unwrap(),
            [earlier, later]
        );
        assert_eq!(read(&file, None).unwrap().len(), 3);
        assert!(
            read(&directory.path().join("missing"), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_history_is_trimmed_to_its_newer_half() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("history.jsonl");
        // Large records, so the limit is reached in about a hundred appends.
        let bulky: Vec<(String, u64)> = (0..TOP_ENTRIES)
            .map(|n| (format!("{n:0>2000}"), 1))
            .collect();
        let top: Vec<(&str, u64)> = bulky
            .iter()
            .map(|(name, size)| (name.as_str(), *size))
            .collect();
        let length = || std::fs::metadata(&file).map_or(0, |metadata| metadata.len());
        let mut day = 0;
        loop {
            let before = length();
            day += 1;
            append(&file, &record("/home", day, day, &top)).unwrap();
            if length() < before {
                break;
            }
            assert!(day < 1_000, "the history never shrank");
        }
        let kept = read(&file, None).unwrap();
        assert!(length() <= MAX_BYTES);
        assert!(kept.len() < day as usize / 2 + 2, "about half was dropped");
        assert_eq!(kept.last().unwrap().size, day, "the newest record survives");
    }

    #[test]
    fn the_previous_record_is_comparable_and_old_enough() {
        let records = [
            record("/home", 1, 100, &[]),
            record("/srv", 2, 5, &[]),
            Record {
                apparent: true,
                ..record("/home", 3, 90, &[])
            },
            record("/home", 4, 150, &[]),
        ];
        // Ten minutes after the latest scan.
        let mut now = record("/home", 4, 160, &[]);
        now.at += Duration::from_secs(600);
        let previous =
            |gap| super::previous(&records, &now, Duration::from_secs(gap)).map(|r| r.size);
        assert_eq!(previous(0), Some(150));
        assert_eq!(
            previous(3600),
            Some(100),
            "skips a scan moments ago and other measures"
        );
        assert_eq!(previous(86_400 * 10), None);
    }

    #[test]
    fn the_largest_change_is_found_among_top_entries() {
        let earlier = record("/home", 1, 0, &[("Library", 100), ("old", 40), ("same", 5)]);
        let later = record("/home", 2, 0, &[("Library", 130), ("new", 60), ("same", 5)]);
        assert_eq!(later.largest_change(&earlier), Some((Path::new("new"), 60)));
        assert_eq!(later.largest_change(&later), None);
    }
}
