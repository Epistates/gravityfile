//! Views across the whole tree: the largest files wherever they are, and
//! space by file type.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

use gravityfile_core::{FileNode, FileTree};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// A file and its path relative to the root.
#[derive(Debug, Clone, Serialize)]
pub struct LargeFile<'a> {
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    pub path: PathBuf,
    pub size: u64,
    #[serde(skip)]
    pub node: &'a FileNode,
}

/// Every file below the root with its path, depth first without recursion,
/// so deep trees cannot exhaust the stack. Stops early when cancelled.
fn for_each_file<'a>(
    tree: &'a FileTree,
    cancel: &CancellationToken,
    mut visit: impl FnMut(&Path, &'a FileNode),
) -> Option<()> {
    let mut path = PathBuf::new();
    let mut open = vec![tree.root.children.iter()];
    let mut visited = 0u32;
    while let Some(children) = open.last_mut() {
        let Some(node) = children.next() else {
            open.pop();
            path.pop();
            continue;
        };
        visited = visited.wrapping_add(1);
        if visited.is_multiple_of(4096) && cancel.is_cancelled() {
            return None;
        }
        path.push(node.file_name());
        if node.is_dir() {
            open.push(node.children.iter());
        } else {
            if node.kind.is_file() {
                visit(&path, node);
            }
            path.pop();
        }
    }
    Some(())
}

/// The `count` largest regular files anywhere below the root, largest first,
/// ties by path. `None` if cancelled.
pub fn largest_files<'a>(
    tree: &'a FileTree,
    count: usize,
    cancel: &CancellationToken,
) -> Option<Vec<LargeFile<'a>>> {
    if count == 0 {
        return Some(Vec::new());
    }
    // A min-heap of the best so far: the smallest kept file is at the top.
    let mut best: BinaryHeap<Reverse<(u64, Reverse<PathBuf>)>> =
        BinaryHeap::with_capacity(count + 1);
    let mut nodes: HashMap<PathBuf, &'a FileNode> = HashMap::with_capacity(count + 1);
    for_each_file(tree, cancel, |path, node| {
        let smallest_kept = best.peek().map(|Reverse((size, _))| *size);
        if best.len() == count && smallest_kept.is_some_and(|smallest| node.size < smallest) {
            return;
        }
        let path = path.to_path_buf();
        nodes.insert(path.clone(), node);
        best.push(Reverse((node.size, Reverse(path))));
        if best.len() > count
            && let Some(Reverse((_, Reverse(dropped)))) = best.pop()
        {
            nodes.remove(&dropped);
        }
    })?;
    let mut files: Vec<LargeFile<'a>> = best
        .into_iter()
        .map(|Reverse((size, Reverse(path)))| {
            let node = nodes[&path];
            LargeFile { path, size, node }
        })
        .collect();
    files.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    Some(files)
}

/// Space taken by one file type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypeStats {
    /// The lowercased extension, or `None` for files without one.
    pub extension: Option<String>,
    pub files: u64,
    pub size: u64,
}

/// Regular files grouped by extension, largest total first. Dotfiles such
/// as `.bashrc` have no extension. `None` if cancelled.
pub fn by_type(tree: &FileTree, cancel: &CancellationToken) -> Option<Vec<TypeStats>> {
    let mut totals: HashMap<Option<String>, (u64, u64)> = HashMap::new();
    for_each_file(tree, cancel, |path, node| {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase());
        let entry = totals.entry(extension).or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(node.size);
    })?;
    let mut types: Vec<TypeStats> = totals
        .into_iter()
        .map(|(extension, (files, size))| TypeStats {
            extension,
            files,
            size,
        })
        .collect();
    types.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    Some(types)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_core::{ScanConfig, Timestamps, TreeStats};

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

    fn tree() -> FileTree {
        let root = dir(
            "root",
            vec![
                file("a.LOG", 50),
                dir(
                    "deep",
                    vec![dir("er", vec![file("big.mp4", 900), file("b.log", 70)])],
                ),
                file(".bashrc", 5),
                file("notes", 70),
            ],
        );
        FileTree::new(
            root,
            PathBuf::from("/r"),
            ScanConfig::new("/r"),
            TreeStats::new(),
            std::time::Duration::ZERO,
            Vec::new(),
        )
    }

    #[test]
    fn the_largest_files_come_from_any_depth_with_ties_by_path() {
        let tree = tree();
        let cancel = CancellationToken::new();
        // Paths compare by component, so `deep/er` also matches `deep\er`.
        let paths = |count| -> Vec<(PathBuf, u64)> {
            largest_files(&tree, count, &cancel)
                .unwrap()
                .into_iter()
                .map(|file| (file.path, file.size))
                .collect()
        };
        assert_eq!(
            paths(3),
            [
                ("deep/er/big.mp4".into(), 900),
                ("deep/er/b.log".into(), 70),
                ("notes".into(), 70),
            ]
        );
        assert_eq!(paths(100).len(), 5);
        assert!(paths(0).is_empty());
    }

    #[test]
    fn types_group_by_lowercased_extension() {
        let types = by_type(&tree(), &CancellationToken::new()).unwrap();
        assert_eq!(
            types,
            [
                TypeStats {
                    extension: Some("mp4".into()),
                    files: 1,
                    size: 900
                },
                TypeStats {
                    extension: Some("log".into()),
                    files: 2,
                    size: 120
                },
                TypeStats {
                    extension: None,
                    files: 2,
                    size: 75
                },
            ]
        );
    }

    #[test]
    fn cancellation_stops_the_walk() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut root = dir("root", Vec::new());
        root.children = (0..5000).map(|n| file(&format!("f{n}"), 1)).collect();
        let tree = FileTree::new(
            root,
            PathBuf::from("/r"),
            ScanConfig::new("/r"),
            TreeStats::new(),
            std::time::Duration::ZERO,
            Vec::new(),
        );
        assert!(by_type(&tree, &cancel).is_none());
        assert!(largest_files(&tree, 3, &cancel).is_none());
    }
}
