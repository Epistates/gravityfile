//! Shared fixtures for the crate's unit tests.
//!
//! Widget tests need `FileNode` trees and app tests need a directory on disk.
//! Both are built here so every test observes the same shapes: a directory with
//! a nested subdirectory, a large file, a small file, an executable, a symlink,
//! and a name wide enough to force truncation.

use std::path::PathBuf;
use std::time::SystemTime;

use gravityfile_core::{FileNode, GitStatus, Timestamps};

/// A fixed timestamp, so nothing in a test depends on the wall clock.
pub fn timestamps() -> Timestamps {
    Timestamps::with_modified(SystemTime::UNIX_EPOCH)
}

/// A regular file node.
pub fn file(name: &str, size: u64) -> FileNode {
    FileNode::new_file(name, size, size.div_ceil(512), timestamps(), false)
}

/// An executable file node.
pub fn executable(name: &str, size: u64) -> FileNode {
    FileNode::new_file(name, size, size.div_ceil(512), timestamps(), true)
}

/// A symlink node.
pub fn symlink(name: &str, target: &str, broken: bool) -> FileNode {
    FileNode::new_symlink(name, target, broken, timestamps())
}

/// A directory node whose size is the sum of its children. (The scanner also
/// adds the directory's own allocation; fixtures leave it at zero.)
pub fn directory(name: &str, children: Vec<FileNode>) -> FileNode {
    let mut node = FileNode::new_directory(name, timestamps());
    node.size = children.iter().map(|child| child.size).sum();
    node.children = children;
    node.finalize();
    node
}

/// A tree deep and varied enough to exercise every display branch: nesting,
/// truncation, git indicators, symlinks, executables, and a zero-size directory
/// (which renders as `...`).
pub fn sample_node_tree() -> FileNode {
    let mut modified = file("modified.rs", 4096);
    modified.git_status = Some(GitStatus::Modified);

    let mut untracked = file("untracked.rs", 1024);
    untracked.git_status = Some(GitStatus::Untracked);

    let nested = directory("nested", vec![file("deep.txt", 128)]);

    let src = directory("src", vec![modified, untracked, nested]);

    // A directory with no children and no size renders the "unknown size" path.
    let unscanned = FileNode::new_directory("unscanned", timestamps());

    directory(
        "root",
        vec![
            src,
            unscanned,
            file("a-very-long-file-name-that-must-be-truncated.log", 65_536),
            executable("run.sh", 512),
            symlink("link", "../elsewhere", false),
            symlink("dangling", "/nowhere", true),
            file("empty", 0),
        ],
    )
}

/// The root path paired with [`sample_node_tree`].
pub fn sample_root_path() -> PathBuf {
    PathBuf::from("/sample")
}

/// A real directory on disk for tests that drive [`crate::app::App`].
///
/// The path is canonicalized by the caller because the scanner stores a
/// canonical root, and on macOS a `TempDir` lives under a `/var` symlink.
pub fn sample_dir_on_disk() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");
    let root = dir.path();

    std::fs::create_dir(root.join("src")).expect("create src");
    std::fs::create_dir(root.join("src/nested")).expect("create nested");
    std::fs::write(root.join("src/nested/deep.txt"), b"deep").expect("write deep");
    std::fs::write(root.join("src/modified.rs"), vec![b'm'; 4096]).expect("write modified");

    // Two identical files so a real duplicate-analysis run finds a group.
    std::fs::write(root.join("copy-a.bin"), vec![7u8; 8192]).expect("write copy-a");
    std::fs::write(root.join("copy-b.bin"), vec![7u8; 8192]).expect("write copy-b");

    std::fs::write(
        root.join("a-very-long-file-name-that-must-be-truncated.log"),
        vec![b'l'; 512],
    )
    .expect("write long name");
    std::fs::write(root.join("empty"), b"").expect("write empty");

    dir
}
