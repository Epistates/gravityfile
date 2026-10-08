//! Reading saved scans: snapshots written by `grav export` and dumps written by
//! `ncdu -o` (or `gdu -o`), either of them optionally zstd-compressed.

use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use gravityfile_core::FileTree;
use serde::Deserialize;

/// Newest snapshot schema this reader understands.
pub const SCHEMA_VERSION: u32 = 2;

/// First bytes of a zstd frame.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// How to measure an imported tree.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Measure apparent lengths instead of allocated blocks.
    pub apparent: bool,
    /// Count every hard link to a file, not only the first.
    pub count_hard_links: bool,
    /// Fail if the snapshot has more than this many entries below the root.
    pub max_entries: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("cannot read snapshot {}: {source}", gravityfile_core::escape_filename(path.as_os_str()))]
    Open { path: PathBuf, source: io::Error },

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("not an ncdu dump: {message} (at byte {offset})")]
    Syntax { offset: u64, message: String },

    #[error("not a gravityfile snapshot: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unsupported snapshot: {0}")]
    Unsupported(String),

    #[error("snapshot has more than {limit} entries")]
    EntryLimitExceeded { limit: u64 },
}

/// Which format a saved scan is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `grav export`
    Gravityfile,
    /// `ncdu -o`
    Ncdu,
}

/// Read the saved scan at `path`, measured as `options` asks.
pub fn read(path: &Path, options: &ImportOptions) -> Result<FileTree, SnapshotError> {
    let open = |source| SnapshotError::Open {
        path: path.to_path_buf(),
        source,
    };
    let file = std::fs::File::open(path).map_err(open)?;
    read_from(BufReader::with_capacity(256 * 1024, file), options)
}

/// Read a saved scan from `input`, decompressing it if it is zstd.
pub fn read_from(
    mut input: impl BufRead,
    options: &ImportOptions,
) -> Result<FileTree, SnapshotError> {
    if input.fill_buf()?.starts_with(&ZSTD_MAGIC) {
        let decoder = zstd::stream::read::Decoder::with_buffer(input)?;
        return read_plain(BufReader::with_capacity(256 * 1024, decoder), options);
    }
    read_plain(input, options)
}

fn read_plain(mut input: impl BufRead, options: &ImportOptions) -> Result<FileTree, SnapshotError> {
    match detect(&mut input)? {
        Format::Ncdu => crate::ncdu::read(input, options),
        Format::Gravityfile => {
            let mut tree = read_gravityfile(input)?;
            if let Some(limit) = options.max_entries {
                let entries = tree.stats.total_files
                    + tree.stats.total_dirs.saturating_sub(1)
                    + tree.stats.total_symlinks;
                if entries > limit {
                    return Err(SnapshotError::EntryLimitExceeded { limit });
                }
            }
            if tree.config.apparent_size != options.apparent
                || tree.config.count_hard_links != options.count_hard_links
            {
                tree.remeasure(options.apparent, options.count_hard_links);
            }
            Ok(tree)
        }
    }
}

/// Tell the formats apart by their first significant byte.
fn detect(input: &mut impl BufRead) -> Result<Format, SnapshotError> {
    loop {
        let buffer = input.fill_buf()?;
        let Some(&first) = buffer.first() else {
            return Err(SnapshotError::Unsupported("the file is empty".into()));
        };
        if first.is_ascii_whitespace() {
            input.consume(1);
            continue;
        }
        return match first {
            b'[' => Ok(Format::Ncdu),
            b'{' => Ok(Format::Gravityfile),
            _ => Err(SnapshotError::Unsupported(
                "expected a grav export or an ncdu dump".into(),
            )),
        };
    }
}

/// A `grav export` document: the versioned envelope, or the bare tree that
/// `grav export` wrote before schema 2. The two are told apart by their first
/// key so the document streams straight into the tree without being buffered.
fn read_gravityfile(mut input: impl BufRead) -> Result<FileTree, SnapshotError> {
    #[derive(Deserialize)]
    struct Envelope {
        schema_version: u32,
        kind: String,
        data: Option<Box<FileTree>>,
    }
    let bare = first_key(&mut input)?.as_deref() == Some(b"root".as_slice());
    let mut json = serde_json::Deserializer::from_reader(input);
    json.disable_recursion_limit();
    // Trees are as deep as the filesystem; grow the stack instead of
    // overflowing it.
    let deserializer = serde_stacker::Deserializer::new(&mut json);
    let tree = if bare {
        FileTree::deserialize(deserializer)?
    } else {
        let envelope = Envelope::deserialize(deserializer)?;
        if envelope.schema_version > SCHEMA_VERSION {
            return Err(SnapshotError::Unsupported(format!(
                "schema version {} is newer than this grav understands ({SCHEMA_VERSION})",
                envelope.schema_version
            )));
        }
        if envelope.kind != "scan" {
            return Err(SnapshotError::Unsupported(format!(
                "a {:?} report, not a scan; save one with `grav export`",
                envelope.kind
            )));
        }
        *envelope
            .data
            .ok_or_else(|| SnapshotError::Unsupported("the document has no data".into()))?
    };
    json.end()?;
    Ok(tree)
}

/// The first key of the object at the start of `input`, if it is visible in
/// the buffered bytes and has no escapes. Nothing is consumed.
fn first_key(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let buffer = input.fill_buf()?;
    let Some(rest) = buffer.strip_prefix(b"{") else {
        return Ok(None);
    };
    let start = rest.iter().position(|byte| !byte.is_ascii_whitespace());
    let Some(rest) = start.and_then(|start| rest[start..].strip_prefix(b"\"")) else {
        return Ok(None);
    };
    Ok(rest
        .iter()
        .position(|&byte| byte == b'"' || byte == b'\\')
        .filter(|&end| rest[end] == b'"')
        .map(|end| rest[..end].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_core::{FileNode, ScanConfig, Timestamps, TreeStats};

    fn tree() -> FileTree {
        let mut root =
            FileNode::new_directory("r", Timestamps::with_modified(std::time::UNIX_EPOCH));
        root.children.push(FileNode::new_file(
            "f",
            1000,
            8,
            Timestamps::with_modified(std::time::UNIX_EPOCH),
            false,
        ));
        root.update_counts();
        let mut tree = FileTree::new(
            root,
            PathBuf::from("/r"),
            ScanConfig::new("/r"),
            TreeStats::new(),
            std::time::Duration::ZERO,
            Vec::new(),
        );
        tree.remeasure(false, false);
        tree
    }

    fn envelope(tree: &FileTree) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "kind": "scan",
            "root": "/r",
            "data": tree,
        }))
        .unwrap()
    }

    #[test]
    fn reads_envelopes_bare_trees_and_ncdu_dumps_alike() {
        let tree = tree();
        let mut dump = Vec::new();
        crate::ncdu::write(&tree, &mut dump).unwrap();
        for document in [envelope(&tree), serde_json::to_vec(&tree).unwrap(), dump] {
            let read = read_from(document.as_slice(), &ImportOptions::default()).unwrap();
            assert_eq!(read.root.size, 4096);
            assert_eq!(read.stats.total_files, 1);
        }
    }

    #[test]
    fn compressed_snapshots_are_detected() {
        let compressed = zstd::encode_all(envelope(&tree()).as_slice(), 3).unwrap();
        let read = read_from(compressed.as_slice(), &ImportOptions::default()).unwrap();
        assert_eq!(read.root.size, 4096);
    }

    #[test]
    fn imports_can_switch_the_measure() {
        let options = ImportOptions {
            apparent: true,
            ..ImportOptions::default()
        };
        let read = read_from(envelope(&tree()).as_slice(), &options).unwrap();
        assert_eq!(read.root.size, 1000);
        assert!(read.config.apparent_size);
    }

    #[test]
    fn deep_snapshots_load_without_overflowing_the_stack() {
        let mut node =
            FileNode::new_directory("d", Timestamps::with_modified(std::time::UNIX_EPOCH));
        for _ in 0..5000 {
            let mut parent =
                FileNode::new_directory("d", Timestamps::with_modified(std::time::UNIX_EPOCH));
            parent.children.push(node);
            node = parent;
        }
        let mut deep = tree();
        deep.root = node;
        let mut bytes = Vec::new();
        {
            // Serialize on a large stack: serde's writer is recursive.
            let deep = &deep;
            std::thread::scope(|scope| {
                std::thread::Builder::new()
                    .stack_size(256 * 1024 * 1024)
                    .spawn_scoped(scope, || bytes = envelope(deep))
                    .unwrap();
            });
        }
        let read = read_from(bytes.as_slice(), &ImportOptions::default()).unwrap();
        let mut depth = 0;
        let mut node = &read.root;
        while let Some(child) = node.children.first() {
            depth += 1;
            node = child;
        }
        assert_eq!(depth, 5000);
    }

    #[test]
    fn rejects_other_reports_and_future_versions() {
        for document in [
            r#"{"schema_version":2,"kind":"age","data":{}}"#,
            r#"{"schema_version":99,"kind":"scan","data":{}}"#,
            "",
            "garbage",
            "{}",
        ] {
            assert!(
                read_from(document.as_bytes(), &ImportOptions::default()).is_err(),
                "{document:?}"
            );
        }
    }
}
