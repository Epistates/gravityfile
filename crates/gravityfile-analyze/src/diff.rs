//! Compare two scans of the same place: what grew, shrank, appeared or went.
//!
//! Entries are matched by name, level by level, and compared by size, so a
//! file rewritten with the same length does not show as a change. Each
//! directory lists its changes largest first, down to a chosen depth; added
//! and removed directories are reported whole rather than entry by entry.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;

use gravityfile_core::FileNode;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// How far a diff goes.
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// Levels below the root to list; 0 lists only the totals.
    pub depth: u32,
    /// Changes listed per directory, largest first; 0 lists all.
    pub limit: usize,
}

/// What happened to an entry between the two scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Removed,
    /// Present in both with a different size, or a different kind.
    Changed,
}

/// One changed entry.
#[derive(Debug, Clone, Serialize)]
pub struct DiffEntry {
    /// Path relative to the root.
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    pub path: PathBuf,
    /// 1 for the root's children.
    pub depth: u32,
    pub change: Change,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_size: Option<u64>,
    /// `new_size - old_size`, missing sides counting as zero.
    pub delta: i64,
    /// Changes in this directory beyond the limit, when its changes are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Omitted>,
}

/// Changes not listed because of the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Omitted {
    pub entries: usize,
    /// Their combined change in size.
    pub delta: i64,
}

/// The result of comparing two trees.
#[derive(Debug, Clone, Serialize)]
pub struct TreeDiff {
    pub old_size: u64,
    pub new_size: u64,
    pub delta: i64,
    /// Depth first, each directory's changes largest first.
    pub entries: Vec<DiffEntry>,
    /// The root's changes beyond the limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Omitted>,
}

/// The change from `old` to `new` bytes, saturating at the `i64` range.
fn delta(old: u64, new: u64) -> i64 {
    if new >= old {
        i64::try_from(new - old).unwrap_or(i64::MAX)
    } else {
        i64::try_from(old - new).map_or(i64::MIN, |shrink| -shrink)
    }
}

/// One child's two sides.
struct Pair<'a> {
    name: &'a OsStr,
    old: Option<&'a FileNode>,
    new: Option<&'a FileNode>,
}

impl Pair<'_> {
    fn delta(&self) -> i64 {
        delta(
            self.old.map_or(0, |node| node.size),
            self.new.map_or(0, |node| node.size),
        )
    }

    fn change(&self) -> Option<Change> {
        match (self.old, self.new) {
            (None, Some(_)) => Some(Change::Added),
            (Some(_), None) => Some(Change::Removed),
            (Some(old), Some(new)) if old.size != new.size || old.is_dir() != new.is_dir() => {
                Some(Change::Changed)
            }
            _ => None,
        }
    }
}

/// Compare the trees rooted at `old` and `new`. `None` if cancelled.
pub fn diff(
    old: &FileNode,
    new: &FileNode,
    options: DiffOptions,
    cancel: &CancellationToken,
) -> Option<TreeDiff> {
    let mut result = TreeDiff {
        old_size: old.size,
        new_size: new.size,
        delta: delta(old.size, new.size),
        entries: Vec::new(),
        omitted: None,
    };
    if options.depth == 0 {
        return Some(result);
    }
    result.omitted = walk(
        old,
        new,
        &mut PathBuf::new(),
        1,
        options,
        &mut result.entries,
        cancel,
    )?;
    Some(result)
}

/// List the changes among the children of a directory present on both
/// sides, and descend into changed directories. Returns the changes beyond
/// the limit, or `None` if cancelled.
fn walk(
    old: &FileNode,
    new: &FileNode,
    path: &mut PathBuf,
    depth: u32,
    options: DiffOptions,
    entries: &mut Vec<DiffEntry>,
    cancel: &CancellationToken,
) -> Option<Option<Omitted>> {
    if cancel.is_cancelled() {
        return None;
    }
    let mut old_children: HashMap<&OsStr, &FileNode> = old
        .children
        .iter()
        .map(|child| (child.file_name(), child))
        .collect();
    let mut pairs: Vec<Pair<'_>> = new
        .children
        .iter()
        .map(|child| {
            let name = child.file_name();
            Pair {
                name,
                old: old_children.remove(name),
                new: Some(child),
            }
        })
        .collect();
    pairs.extend(old_children.into_values().map(|child| Pair {
        name: child.file_name(),
        old: Some(child),
        new: None,
    }));
    let mut changed: Vec<(Pair<'_>, Change)> = pairs
        .into_iter()
        .filter_map(|pair| pair.change().map(|change| (pair, change)))
        .collect();
    changed.sort_by(|(a, _), (b, _)| {
        b.delta()
            .unsigned_abs()
            .cmp(&a.delta().unsigned_abs())
            .then_with(|| a.name.cmp(b.name))
    });
    let shown = if options.limit == 0 {
        changed.len()
    } else {
        options.limit.min(changed.len())
    };
    let rest = &changed[shown..];
    let omitted = (!rest.is_empty()).then(|| Omitted {
        entries: rest.len(),
        delta: rest
            .iter()
            .fold(0i64, |sum, (pair, _)| sum.saturating_add(pair.delta())),
    });
    for (pair, change) in &changed[..shown] {
        path.push(pair.name);
        let index = entries.len();
        entries.push(DiffEntry {
            path: path.clone(),
            depth,
            change: *change,
            is_dir: pair.new.or(pair.old).is_some_and(FileNode::is_dir),
            old_size: pair.old.map(|node| node.size),
            new_size: pair.new.map(|node| node.size),
            delta: pair.delta(),
            omitted: None,
        });
        // Added and removed directories are reported whole.
        if depth < options.depth
            && let (Some(old), Some(new)) = (pair.old, pair.new)
            && old.is_dir()
            && new.is_dir()
        {
            entries[index].omitted = walk(old, new, path, depth + 1, options, entries, cancel)?;
        }
        path.pop();
    }
    Some(omitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_core::Timestamps;

    fn file(name: &str, size: u64) -> FileNode {
        FileNode::new_file(
            name,
            size,
            size.div_ceil(512),
            Timestamps::with_modified(std::time::UNIX_EPOCH),
            false,
        )
    }

    fn dir(name: &str, children: Vec<FileNode>) -> FileNode {
        let mut node =
            FileNode::new_directory(name, Timestamps::with_modified(std::time::UNIX_EPOCH));
        node.size = children.iter().map(|child| child.size).sum();
        node.children = children;
        node
    }

    fn options(depth: u32, limit: usize) -> DiffOptions {
        DiffOptions { depth, limit }
    }

    fn run(old: &FileNode, new: &FileNode, options: DiffOptions) -> TreeDiff {
        diff(old, new, options, &CancellationToken::new()).unwrap()
    }

    /// Paths compare by component, so `cache/a` also matches `cache\a`.
    fn summary(diff: &TreeDiff) -> Vec<(PathBuf, Change, i64)> {
        diff.entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.change, entry.delta))
            .collect()
    }

    #[test]
    fn changes_are_listed_largest_first_and_unchanged_entries_are_left_out() {
        let old = dir(
            "root",
            vec![
                file("same", 10),
                file("grew", 100),
                file("gone", 50),
                dir("cache", vec![file("a", 1_000), file("b", 5)]),
            ],
        );
        let new = dir(
            "root",
            vec![
                file("same", 10),
                file("grew", 400),
                file("new", 20),
                dir("cache", vec![file("a", 10), file("b", 5), file("c", 7)]),
            ],
        );
        let result = run(&old, &new, options(2, 0));
        assert_eq!(
            (result.old_size, result.new_size, result.delta),
            (1_165, 452, -713)
        );
        assert_eq!(
            summary(&result),
            [
                ("cache".into(), Change::Changed, -983),
                ("cache/a".into(), Change::Changed, -990),
                ("cache/c".into(), Change::Added, 7),
                ("grew".into(), Change::Changed, 300),
                ("gone".into(), Change::Removed, -50),
                ("new".into(), Change::Added, 20),
            ]
        );
    }

    #[test]
    fn depth_and_limit_bound_the_listing() {
        let old = dir(
            "root",
            vec![dir("d", vec![file("x", 1)]), file("a", 1), file("b", 1)],
        );
        let new = dir(
            "root",
            vec![dir("d", vec![file("x", 9)]), file("a", 3), file("b", 4)],
        );
        let result = run(&old, &new, options(1, 2));
        assert_eq!(
            summary(&result),
            [
                ("d".into(), Change::Changed, 8),
                ("b".into(), Change::Changed, 3),
            ]
        );
        assert_eq!(
            result.omitted,
            Some(Omitted {
                entries: 1,
                delta: 2
            })
        );
        assert!(run(&old, &new, options(0, 0)).entries.is_empty());
    }

    #[test]
    fn added_and_removed_directories_are_reported_whole() {
        let old = dir("root", vec![dir("old", vec![file("x", 5)])]);
        let new = dir("root", vec![dir("new", vec![file("y", 6)])]);
        let result = run(&old, &new, options(5, 0));
        assert_eq!(
            summary(&result),
            [
                ("new".into(), Change::Added, 6),
                ("old".into(), Change::Removed, -5),
            ]
        );
        assert!(result.entries.iter().all(|entry| entry.is_dir));
    }

    #[test]
    fn a_file_replaced_by_a_directory_of_the_same_size_is_a_change() {
        let old = dir("root", vec![file("thing", 5)]);
        let new = dir("root", vec![dir("thing", vec![file("inside", 5)])]);
        assert_eq!(
            summary(&run(&old, &new, options(1, 0))),
            [("thing".into(), Change::Changed, 0)]
        );
    }

    #[test]
    fn cancellation_stops_the_comparison() {
        let tree = dir("root", vec![file("a", 1)]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(diff(&tree, &tree, options(1, 0), &cancel).is_none());
    }
}
