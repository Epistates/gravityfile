//! Iterative CLI tree encoding. Scalar/metadata fields keep their Serde codecs.

use std::io::Write;
use std::path::Path;

use color_eyre::Result;
use gravityfile_core::{FileNode, FileTree};
use serde::Serialize;

#[derive(Serialize)]
struct NativePath<'a>(
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")] &'a Path,
);

fn field(writer: &mut dyn Write, name: &str, value: &impl Serialize) -> Result<()> {
    writer.write_all(b",")?;
    serde_json::to_writer(&mut *writer, name)?;
    writer.write_all(b":")?;
    serde_json::to_writer(writer, value)?;
    Ok(())
}

fn begin_node(writer: &mut dyn Write, node: &FileNode) -> Result<()> {
    writer.write_all(b"{\"id\":")?;
    serde_json::to_writer(&mut *writer, &node.id)?;
    field(writer, "name", &node.name)?;
    if node.native_name.is_some() {
        field(writer, "native_name", &node.native_name)?;
    }
    field(writer, "kind", &node.kind)?;
    field(writer, "size", &node.size)?;
    field(writer, "blocks", &node.blocks)?;
    field(writer, "timestamps", &node.timestamps)?;
    field(writer, "inode", &node.inode)?;
    field(writer, "content_hash", &node.content_hash)?;
    field(writer, "git_status", &node.git_status)?;
    writer.write_all(b",\"children\":[")?;
    Ok(())
}

fn tree(writer: &mut dyn Write, tree: &FileTree) -> Result<()> {
    writer.write_all(b"{\"root\":")?;
    begin_node(writer, &tree.root)?;
    let mut stack = vec![(tree.root.children.iter(), true)];
    while let Some((children, first)) = stack.last_mut() {
        if let Some(child) = children.next() {
            if !*first {
                writer.write_all(b",")?;
            }
            *first = false;
            begin_node(writer, child)?;
            stack.push((child.children.iter(), true));
        } else {
            writer.write_all(b"]}")?;
            stack.pop();
        }
    }
    field(writer, "root_path", &NativePath(&tree.root_path))?;
    field(writer, "scanned_at", &tree.scanned_at)?;
    field(writer, "scan_duration", &tree.scan_duration)?;
    field(writer, "config", &tree.config)?;
    field(writer, "stats", &tree.stats)?;
    field(writer, "warnings", &tree.warnings)?;
    writer.write_all(b"}")?;
    Ok(())
}

pub(super) fn write(
    writer: &mut dyn Write,
    value: &FileTree,
    envelope: Option<&super::ReportEnvelope<'_, FileTree>>,
) -> Result<()> {
    if let Some(envelope) = envelope {
        writer.write_all(b"{\"schema_version\":")?;
        serde_json::to_writer(&mut *writer, &envelope.schema_version)?;
        field(writer, "tool_version", &envelope.tool_version)?;
        field(writer, "kind", &envelope.kind)?;
        field(writer, "root", &NativePath(envelope.root))?;
        field(writer, "scope", &envelope.scope)?;
        field(writer, "parameters", &envelope.parameters)?;
        field(writer, "status", &envelope.status)?;
        writer.write_all(b",\"data\":")?;
        tree(writer, value)?;
        writer.write_all(b"}")?;
    } else {
        tree(writer, value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_core::{ContentHash, GitStatus, NodeId, NodeKind, Timestamps};
    use tokio_util::sync::CancellationToken;

    fn fixture() -> FileTree {
        serde_json::from_str(include_str!("../tests/fixtures/reports/legacy-scan.json")).unwrap()
    }

    fn compare(tree: &FileTree) {
        let status = crate::scan_status(tree);
        let envelope = crate::ReportEnvelope::new("scan", tree, &status, tree);
        for wrapped in [false, true] {
            let mut output = Vec::new();
            write(&mut output, tree, wrapped.then_some(&envelope)).unwrap();
            let actual: serde_json::Value = serde_json::from_slice(&output).unwrap();
            let expected = if wrapped {
                serde_json::to_value(&envelope).unwrap()
            } else {
                serde_json::to_value(tree).unwrap()
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn iterative_encoding_preserves_legacy_schema_and_node_variants() {
        let mut tree = fixture();
        compare(&tree);
        for kind in [
            NodeKind::File { executable: true },
            NodeKind::Symlink {
                target: "../target\n\"".into(),
                broken: true,
            },
            NodeKind::Other,
        ] {
            let mut child = tree.root.children[0].clone();
            child.kind = kind;
            child.git_status = Some(GitStatus::Conflict);
            child.content_hash = Some(ContentHash::new([255; 32]));
            tree.root.children.push(child);
        }
        compare(&tree);
    }

    #[cfg(unix)]
    #[test]
    fn iterative_encoding_preserves_native_names_and_paths() {
        use std::os::unix::ffi::OsStrExt;
        let mut tree = fixture();
        let native = std::ffi::OsStr::from_bytes(b"native-\xff");
        tree.root.children[0].set_file_name(native);
        tree.root_path = Path::new(native).to_path_buf();
        compare(&tree);
    }

    #[test]
    fn staged_deep_tree_encoding_uses_a_small_call_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut tree = fixture();
                let mut root = FileNode::new_directory(
                    NodeId::new(0),
                    "d",
                    Timestamps::with_modified(std::time::UNIX_EPOCH),
                );
                for id in 1..10_000 {
                    let mut parent = FileNode::new_directory(
                        NodeId::new(id),
                        "d",
                        Timestamps::with_modified(std::time::UNIX_EPOCH),
                    );
                    parent.children.push(root);
                    root = parent;
                }
                tree.root = root;
                let mut output = Vec::new();
                crate::write_json_report_with(&mut output, &CancellationToken::new(), |writer| {
                    write(writer, &tree, None)
                })
                .unwrap();
                let text = std::str::from_utf8(&output).unwrap();
                assert_eq!(text.matches("\"children\":[").count(), 10_000);
                assert!(text.ends_with("}\n"));
                assert!(text.contains("\"scan_duration\":"));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn failed_tree_encoding_preserves_stdout_and_existing_export() {
        let tree = fixture();
        let token = CancellationToken::new();
        let encode = |writer: &mut dyn Write| -> Result<()> {
            write(writer, &tree, None)?;
            token.cancel();
            Ok(())
        };
        let mut output = Vec::new();
        assert!(crate::write_json_report_with(&mut output, &token, encode).is_err());
        assert!(output.is_empty());
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("report.json");
        std::fs::write(&destination, b"previous").unwrap();
        let token = CancellationToken::new();
        assert!(
            crate::write_json_atomic_with(&destination, &token, |writer| {
                write(writer, &tree, None)?;
                token.cancel();
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"previous");
    }
}
