//! The ncdu JSON dump format (<https://dev.yorhel.nl/ncdu/jsonfmt>), as
//! written by `ncdu -o` and `gdu -o`.
//!
//! A dump is `[1, 2, {metadata}, directory]`, where a directory is an array of
//! its own info object followed by its children: nested arrays for
//! directories, objects for everything else. Each entry carries its own
//! apparent size (`asize`) and disk usage (`dsize`); readers aggregate them.
//! Names are raw bytes, not necessarily UTF-8, so both directions work on
//! bytes rather than through a JSON library. Both are iterative and stream, so
//! neither nesting depth nor dump size is limited by the stack or by holding
//! the document in memory.

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gravityfile_core::{
    FileNode, FileTree, InodeInfo, NodeKind, ScanConfig, ScanWarning, Timestamps, TreeStats,
    WarningKind,
};

use crate::snapshot::{ImportOptions, SnapshotError};

/// Longest name the format allows.
const MAX_NAME: usize = 32 * 1024;

/// Longest string accepted in fields this reader ignores.
const SKIPPED_STRING: usize = 1 << 20;

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Write `tree` as an ncdu dump, format version 1.2.
///
/// Entries whose path appears among the tree's warnings are marked
/// `read_error`. Hard-linked files carry `ino`, `dev` and `hlnkc`, so readers
/// count them once.
pub fn write(tree: &FileTree, out: &mut dyn Write) -> io::Result<()> {
    let timestamp = tree
        .scanned_at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    writeln!(
        out,
        "[1,2,{{\"progname\":\"gravityfile\",\"progver\":\"{}\",\"timestamp\":{timestamp}}},",
        env!("CARGO_PKG_VERSION")
    )?;
    let failed: HashSet<&Path> = tree
        .warnings
        .iter()
        .filter(|warning| warning.kind != WarningKind::BrokenSymlink)
        .map(|warning| warning.path.as_path())
        .collect();
    let mut path = tree.root_path.clone();
    out.write_all(b"[")?;
    write_info(
        out,
        &tree.root,
        tree.root_path.as_os_str(),
        failed.contains(path.as_path()),
    )?;
    let mut stack = vec![tree.root.children.iter()];
    while let Some(children) = stack.last_mut() {
        let Some(node) = children.next() else {
            out.write_all(b"]")?;
            stack.pop();
            path.pop();
            continue;
        };
        out.write_all(b",\n")?;
        path.push(node.file_name());
        let failed = failed.contains(path.as_path());
        if node.is_dir() {
            out.write_all(b"[")?;
            write_info(out, node, node.file_name(), failed)?;
            stack.push(node.children.iter());
        } else {
            write_info(out, node, node.file_name(), failed)?;
            path.pop();
        }
    }
    out.write_all(b"]\n")?;
    out.flush()
}

fn write_info(
    out: &mut dyn Write,
    node: &FileNode,
    name: &std::ffi::OsStr,
    read_error: bool,
) -> io::Result<()> {
    out.write_all(b"{\"name\":")?;
    write_bytes(out, name.as_encoded_bytes())?;
    write!(
        out,
        ",\"asize\":{},\"dsize\":{}",
        node.len,
        node.blocks.saturating_mul(512)
    )?;
    if let Some(identity) = node.inode {
        write!(
            out,
            ",\"dev\":{},\"ino\":{},\"hlnkc\":true",
            identity.device, identity.inode
        )?;
    }
    if matches!(node.kind, NodeKind::Symlink { .. } | NodeKind::Other) {
        out.write_all(b",\"notreg\":true")?;
    }
    if read_error {
        out.write_all(b",\"read_error\":true")?;
    }
    if let Ok(modified) = node.timestamps.modified.duration_since(UNIX_EPOCH) {
        write!(out, ",\"mtime\":{}", modified.as_secs())?;
    }
    out.write_all(b"}")
}

/// A JSON string of raw bytes: quotes, backslashes and control bytes are
/// escaped; everything else, including bytes that are not UTF-8, is written
/// as is, as ncdu does.
fn write_bytes(out: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(b"\"")?;
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'"' || byte == b'\\' || byte < 0x20 {
            out.write_all(&bytes[start..index])?;
            match byte {
                b'"' => out.write_all(b"\\\"")?,
                b'\\' => out.write_all(b"\\\\")?,
                b'\n' => out.write_all(b"\\n")?,
                b'\t' => out.write_all(b"\\t")?,
                _ => write!(out, "\\u{byte:04x}")?,
            }
            start = index + 1;
        }
    }
    out.write_all(&bytes[start..])?;
    out.write_all(b"\"")
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read an ncdu dump into a tree measured as `options` asks.
pub fn read(input: impl BufRead, options: &ImportOptions) -> Result<FileTree, SnapshotError> {
    let mut parser = Parser {
        input,
        offset: 0,
        entries: 0,
        max_entries: options.max_entries,
    };
    parser.expect(b'[')?;
    let major = parser.integer()?;
    if major != 1 {
        return Err(parser.error(format!("unsupported ncdu dump version {major}")));
    }
    parser.expect(b',')?;
    parser.integer()?;
    parser.expect(b',')?;
    let scanned_at = parser.metadata()?;
    parser.expect(b',')?;
    let (root, root_path, warnings) = parser.tree()?;
    parser.expect(b']')?;
    if parser.peek()?.is_some() {
        return Err(parser.error("data after the end of the dump".into()));
    }

    let mut config = ScanConfig::new(&root_path);
    config.apparent_size = options.apparent;
    config.count_hard_links = options.count_hard_links;
    let mut tree = FileTree::new(
        root,
        root_path,
        config,
        TreeStats::new(),
        Duration::ZERO,
        warnings,
    );
    tree.root.update_counts();
    tree.remeasure(options.apparent, options.count_hard_links);
    if let Some(scanned_at) = scanned_at {
        tree.scanned_at = scanned_at;
    }
    Ok(tree)
}

/// What an info object says about one entry.
#[derive(Default)]
struct Info {
    name: Option<Vec<u8>>,
    asize: u64,
    dsize: u64,
    dev: Option<u64>,
    ino: Option<u64>,
    nlink: u64,
    hlnkc: bool,
    notreg: bool,
    read_error: bool,
    excluded: bool,
    mtime: Option<u64>,
    mode: Option<u64>,
}

struct Parser<R> {
    input: R,
    offset: u64,
    entries: u64,
    max_entries: Option<u64>,
}

/// A directory whose entries are still being read.
struct Open {
    node: FileNode,
    dev: u64,
}

impl<R: BufRead> Parser<R> {
    fn error(&self, message: String) -> SnapshotError {
        SnapshotError::Syntax {
            offset: self.offset,
            message,
        }
    }

    fn peek_raw(&mut self) -> Result<Option<u8>, SnapshotError> {
        Ok(self.input.fill_buf()?.first().copied())
    }

    fn bump(&mut self) {
        self.input.consume(1);
        self.offset += 1;
    }

    /// The next significant byte, without consuming it.
    fn peek(&mut self) -> Result<Option<u8>, SnapshotError> {
        while let Some(byte) = self.peek_raw()? {
            if !byte.is_ascii_whitespace() {
                return Ok(Some(byte));
            }
            self.bump();
        }
        Ok(None)
    }

    fn next(&mut self) -> Result<u8, SnapshotError> {
        match self.peek()? {
            Some(byte) => {
                self.bump();
                Ok(byte)
            }
            None => Err(self.error("unexpected end of dump".into())),
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), SnapshotError> {
        let found = self.next()?;
        if found == wanted {
            Ok(())
        } else {
            Err(self.error(format!(
                "expected '{}', found '{}'",
                wanted as char,
                found.escape_ascii()
            )))
        }
    }

    /// The raw text of a number.
    fn number_text(&mut self) -> Result<String, SnapshotError> {
        self.peek()?;
        let mut text = String::new();
        while let Some(byte) = self.peek_raw()? {
            if !(byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')) {
                break;
            }
            if text.len() >= 64 {
                return Err(self.error("number too long".into()));
            }
            text.push(byte as char);
            self.bump();
        }
        if text.is_empty() {
            return Err(self.error("expected a number".into()));
        }
        Ok(text)
    }

    fn integer(&mut self) -> Result<u64, SnapshotError> {
        let text = self.number_text()?;
        text.parse()
            .map_err(|_| self.error(format!("expected a non-negative integer, found {text}")))
    }

    /// Integral part of a non-negative number, which may be fractional.
    fn whole_number(&mut self) -> Result<u64, SnapshotError> {
        let text = self.number_text()?;
        if let Ok(value) = text.parse::<u64>() {
            return Ok(value);
        }
        match text.parse::<f64>() {
            Ok(value) if value.is_finite() && value >= 0.0 && value < u64::MAX as f64 => {
                Ok(value as u64)
            }
            _ => Err(self.error(format!("expected a non-negative number, found {text}"))),
        }
    }

    fn boolean(&mut self) -> Result<bool, SnapshotError> {
        match self.next()? {
            b't' => self.literal(b"rue").map(|()| true),
            b'f' => self.literal(b"alse").map(|()| false),
            b'n' => self.literal(b"ull").map(|()| false),
            other => Err(self.error(format!(
                "expected a boolean, found '{}'",
                other.escape_ascii()
            ))),
        }
    }

    fn literal(&mut self, rest: &[u8]) -> Result<(), SnapshotError> {
        for &wanted in rest {
            match self.peek_raw()? {
                Some(byte) if byte == wanted => self.bump(),
                _ => return Err(self.error("invalid literal".into())),
            }
        }
        Ok(())
    }

    /// A string's bytes, with escapes decoded and raw bytes kept as they are.
    fn string(&mut self, limit: usize) -> Result<Vec<u8>, SnapshotError> {
        self.expect(b'"')?;
        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.peek_raw()? else {
                return Err(self.error("unterminated string".into()));
            };
            self.bump();
            match byte {
                b'"' => return Ok(bytes),
                b'\\' => {
                    let Some(escape) = self.peek_raw()? else {
                        return Err(self.error("unterminated string".into()));
                    };
                    self.bump();
                    match escape {
                        b'"' | b'\\' | b'/' => bytes.push(escape),
                        b'b' => bytes.push(0x08),
                        b'f' => bytes.push(0x0c),
                        b'n' => bytes.push(b'\n'),
                        b'r' => bytes.push(b'\r'),
                        b't' => bytes.push(b'\t'),
                        b'u' => {
                            let character = self.unicode_escape()?;
                            let mut buffer = [0; 4];
                            bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                        }
                        other => {
                            return Err(
                                self.error(format!("invalid escape '\\{}'", other.escape_ascii()))
                            );
                        }
                    }
                }
                _ => bytes.push(byte),
            }
            if bytes.len() > limit {
                return Err(self.error(format!("string longer than {limit} bytes")));
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, SnapshotError> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = self
                .peek_raw()?
                .and_then(|byte| (byte as char).to_digit(16))
                .ok_or_else(|| self.error("invalid \\u escape".into()))?;
            self.bump();
            value = value * 16 + digit;
        }
        Ok(value)
    }

    /// The character of a `\u` escape, joining surrogate pairs. Lone
    /// surrogates cannot be represented and become U+FFFD.
    fn unicode_escape(&mut self) -> Result<char, SnapshotError> {
        let first = self.hex4()?;
        if (0xd800..0xdc00).contains(&first) && self.peek_raw()? == Some(b'\\') {
            self.bump();
            if self.peek_raw()? != Some(b'u') {
                return Err(self.error("invalid surrogate pair".into()));
            }
            self.bump();
            let second = self.hex4()?;
            if (0xdc00..0xe000).contains(&second) {
                let code = 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
                return Ok(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            return Ok(char::REPLACEMENT_CHARACTER);
        }
        Ok(char::from_u32(first).unwrap_or(char::REPLACEMENT_CHARACTER))
    }

    /// Skip any value, iteratively.
    fn skip_value(&mut self) -> Result<(), SnapshotError> {
        // The closing byte of each open container.
        let mut open = Vec::new();
        loop {
            match self.peek()? {
                Some(byte @ (b'[' | b'{')) => {
                    self.bump();
                    let close = if byte == b'[' { b']' } else { b'}' };
                    if self.peek()? == Some(close) {
                        self.bump();
                    } else {
                        open.push(close);
                        if close == b'}' {
                            self.key()?;
                        }
                        continue;
                    }
                }
                Some(b'"') => {
                    self.string(SKIPPED_STRING)?;
                }
                Some(b't' | b'f' | b'n') => {
                    self.boolean()?;
                }
                Some(_) => {
                    self.number_text()?;
                }
                None => return Err(self.error("unexpected end of dump".into())),
            }
            // After a value: close containers, or move on to the next value.
            loop {
                let Some(&close) = open.last() else {
                    return Ok(());
                };
                match self.next()? {
                    b',' => {
                        if close == b'}' {
                            self.key()?;
                        }
                        break;
                    }
                    byte if byte == close => {
                        open.pop();
                    }
                    other => {
                        return Err(self.error(format!("unexpected '{}'", other.escape_ascii())));
                    }
                }
            }
        }
    }

    /// An object key and its colon.
    fn key(&mut self) -> Result<(), SnapshotError> {
        self.string(SKIPPED_STRING)?;
        self.expect(b':')
    }

    /// The metadata object; returns its timestamp.
    fn metadata(&mut self) -> Result<Option<SystemTime>, SnapshotError> {
        self.expect(b'{')?;
        let mut timestamp = None;
        if self.peek()? == Some(b'}') {
            self.bump();
            return Ok(None);
        }
        loop {
            let key = self.string(256)?;
            self.expect(b':')?;
            if key == b"timestamp" {
                timestamp = since_epoch(self.whole_number()?);
            } else {
                self.skip_value()?;
            }
            match self.next()? {
                b',' => continue,
                b'}' => return Ok(timestamp),
                other => return Err(self.error(format!("unexpected '{}'", other.escape_ascii()))),
            }
        }
    }

    fn info(&mut self) -> Result<Info, SnapshotError> {
        self.expect(b'{')?;
        let mut info = Info {
            nlink: 1,
            ..Info::default()
        };
        if self.peek()? == Some(b'}') {
            self.bump();
            return Ok(info);
        }
        loop {
            let key = self.string(256)?;
            self.expect(b':')?;
            match key.as_slice() {
                b"name" => info.name = Some(self.string(MAX_NAME)?),
                b"asize" => info.asize = self.integer()?,
                b"dsize" => info.dsize = self.integer()?,
                b"dev" => info.dev = Some(self.integer()?),
                b"ino" => info.ino = Some(self.integer()?),
                b"nlink" => info.nlink = self.integer()?,
                b"hlnkc" => info.hlnkc = self.boolean()?,
                b"notreg" => info.notreg = self.boolean()?,
                b"read_error" => info.read_error = self.boolean()?,
                b"mtime" => info.mtime = Some(self.whole_number()?),
                b"mode" => info.mode = Some(self.integer()?),
                b"excluded" => {
                    info.excluded = if self.peek()? == Some(b'"') {
                        self.string(256)?;
                        true
                    } else {
                        self.boolean()?
                    }
                }
                _ => self.skip_value()?,
            }
            match self.next()? {
                b',' => continue,
                b'}' => return Ok(info),
                other => return Err(self.error(format!("unexpected '{}'", other.escape_ascii()))),
            }
        }
    }

    /// A node for `info`, inside a directory on device `dev`.
    fn node(&self, info: &Info, directory: bool, dev: u64) -> Result<FileNode, SnapshotError> {
        let name = info
            .name
            .as_deref()
            .ok_or_else(|| self.error("entry without a name".into()))?;
        let modified = info.mtime.and_then(since_epoch).unwrap_or(UNIX_EPOCH);
        let timestamps = Timestamps::with_modified(modified);
        let mut node = if directory {
            FileNode::new_directory("", timestamps)
        } else {
            let executable = info.mode.is_some_and(|mode| mode & 0o111 != 0);
            let mut node = FileNode::new_file("", 0, 0, timestamps, executable);
            if info.notreg {
                node.kind = NodeKind::Other;
            }
            node
        };
        node.set_file_name(&bytes_to_os(name));
        node.len = info.asize;
        node.blocks = info.dsize.div_ceil(512);
        if !directory
            && (info.hlnkc || info.nlink > 1)
            && let Some(inode) = info.ino
        {
            node.inode = Some(InodeInfo::new(inode, info.dev.unwrap_or(dev)));
        }
        Ok(node)
    }

    fn count_entry(&mut self) -> Result<(), SnapshotError> {
        if let Some(limit) = self.max_entries
            && self.entries >= limit
        {
            return Err(SnapshotError::EntryLimitExceeded { limit });
        }
        self.entries += 1;
        Ok(())
    }

    /// The root directory array and everything in it.
    fn tree(&mut self) -> Result<(FileNode, PathBuf, Vec<ScanWarning>), SnapshotError> {
        let mut warnings = Vec::new();
        self.expect(b'[')?;
        let info = self.info()?;
        let root_path = PathBuf::from(bytes_to_os(info.name.as_deref().unwrap_or_default()));
        if root_path.as_os_str().is_empty() {
            return Err(self.error("the root directory has no name".into()));
        }
        let dev = info.dev.unwrap_or(0);
        let mut root = self.node(&info, true, dev)?;
        root.set_file_name(root_path.file_name().unwrap_or(root_path.as_os_str()));
        if info.read_error {
            warnings.push(read_error(&root_path));
        }
        let mut path = root_path.clone();
        let mut stack = vec![Open { node: root, dev }];
        loop {
            match self.next()? {
                b',' => {}
                b']' => {
                    let finished = stack.pop().expect("open directory");
                    let Some(parent) = stack.last_mut() else {
                        return Ok((finished.node, root_path, warnings));
                    };
                    parent.node.children.push(finished.node);
                    path.pop();
                    continue;
                }
                other => return Err(self.error(format!("unexpected '{}'", other.escape_ascii()))),
            }
            let parent_dev = stack.last().expect("open directory").dev;
            self.count_entry()?;
            if self.peek()? == Some(b'[') {
                self.bump();
                let info = self.info()?;
                let node = self.node(&info, true, parent_dev)?;
                path.push(node.file_name());
                if info.read_error {
                    warnings.push(read_error(&path));
                }
                let dev = info.dev.unwrap_or(parent_dev);
                if info.excluded {
                    // Keep the structure consistent: read and drop its contents.
                    self.skip_directory_rest()?;
                    path.pop();
                    continue;
                }
                stack.push(Open { node, dev });
            } else {
                let info = self.info()?;
                if info.excluded {
                    continue;
                }
                let node = self.node(&info, false, parent_dev)?;
                if info.read_error {
                    warnings.push(read_error(&path.join(node.file_name())));
                }
                stack
                    .last_mut()
                    .expect("open directory")
                    .node
                    .children
                    .push(node);
            }
        }
    }

    /// Skip the remaining entries of a directory array whose info object was
    /// just read.
    fn skip_directory_rest(&mut self) -> Result<(), SnapshotError> {
        loop {
            match self.next()? {
                b',' => self.skip_value()?,
                b']' => return Ok(()),
                other => return Err(self.error(format!("unexpected '{}'", other.escape_ascii()))),
            }
        }
    }
}

fn read_error(path: &Path) -> ScanWarning {
    ScanWarning::new(
        path,
        WarningKind::ReadError,
        "could not be read when the dump was made",
    )
}

/// The time `secs` after the epoch, or `None` past what the platform can
/// represent: a corrupt timestamp is unknown, not a crash.
fn since_epoch(secs: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

#[cfg(unix)]
fn bytes_to_os(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn bytes_to_os(bytes: &[u8]) -> std::ffi::OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(dump: &str, apparent: bool) -> FileTree {
        read(
            dump.as_bytes(),
            &ImportOptions {
                apparent,
                ..ImportOptions::default()
            },
        )
        .unwrap()
    }

    fn child<'a>(node: &'a FileNode, name: &str) -> &'a FileNode {
        node.children
            .iter()
            .find(|child| child.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    const SAMPLE: &str = r#"[1,2,{"progname":"ncdu","progver":"2.4","timestamp":1700000000},
        [{"name":"/data","asize":4096,"dsize":4096,"dev":12},
         {"name":"a.bin","asize":1000,"dsize":4096,"mtime":1600000000.5},
         [{"name":"sub","asize":4096,"dsize":4096},
          {"name":"linked","asize":500,"dsize":4096,"ino":7,"nlink":2,"hlnkc":true},
          {"name":"fifo","notreg":true},
          {"name":"skipped","asize":99999,"excluded":"pattern"},
          [{"name":"gone","excluded":"otherfs"},{"name":"inside","asize":5}]],
         {"name":"linked-too","asize":500,"dsize":4096,"ino":7,"nlink":2,"hlnkc":true},
         {"name":"bad","read_error":true,"future":{"nested":[1,{"x":[]}]}}]]"#;

    #[test]
    fn reads_sizes_hard_links_and_exclusions() {
        let tree = import(SAMPLE, false);
        assert_eq!(tree.root_path, Path::new("/data"));
        assert_eq!(tree.root.name, "data");
        assert_eq!(
            tree.root.size,
            4096 * 4,
            "root, a.bin, sub, one copy of linked"
        );
        assert_eq!(child(&tree.root, "a.bin").size, 4096);
        let sub = child(&tree.root, "sub");
        assert!(matches!(child(sub, "fifo").kind, NodeKind::Other));
        assert!(
            sub.children
                .iter()
                .all(|node| node.name != "skipped" && node.name != "gone")
        );
        assert_eq!(
            tree.stats.total_files, 4,
            "a.bin, both links and bad; the fifo is not a regular file"
        );
        assert_eq!(
            tree.warnings
                .iter()
                .map(|w| w.path.clone())
                .collect::<Vec<_>>(),
            [PathBuf::from("/data/bad")]
        );
        assert_eq!(
            tree.scanned_at,
            UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        );
        assert_eq!(
            child(&tree.root, "a.bin").timestamps.modified,
            UNIX_EPOCH + Duration::from_secs(1_600_000_000)
        );

        let tree = import(SAMPLE, true);
        assert_eq!(tree.root.size, 4096 + 1000 + 4096 + 500);
    }

    #[test]
    fn writing_and_reading_back_preserves_the_tree() {
        let original = import(SAMPLE, false);
        let mut dump = Vec::new();
        write(&original, &mut dump).unwrap();
        let restored = read(dump.as_slice(), &ImportOptions::default()).unwrap();
        assert_eq!(restored.root.size, original.root.size);
        assert_eq!(restored.stats.total_files, original.stats.total_files);
        assert_eq!(restored.root_path, original.root_path);
        assert_eq!(restored.warnings.len(), 1);
        let restored_sub = child(&restored.root, "sub");
        assert_eq!(restored_sub.size, child(&original.root, "sub").size);
        assert!(matches!(child(restored_sub, "fifo").kind, NodeKind::Other));
    }

    #[cfg(unix)]
    #[test]
    fn names_round_trip_as_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut tree = import(SAMPLE, false);
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff \"quoted\" back\\slash\nline");
        tree.root.children[0].set_file_name(name);
        let mut dump = Vec::new();
        write(&tree, &mut dump).unwrap();
        assert!(dump.windows(5).any(|window| window == b"bad-\xff"));
        let restored = read(dump.as_slice(), &ImportOptions::default()).unwrap();
        assert!(
            restored
                .root
                .children
                .iter()
                .any(|node| node.file_name() == name)
        );
    }

    #[test]
    fn decodes_escapes_and_surrogate_pairs() {
        let dump = r#"[1,2,{},[{"name":"/r"},{"name":"\u00e9\ud83d\ude00\t\/","asize":1}]]"#;
        let tree = import(dump, true);
        assert_eq!(tree.root.children[0].name, "é😀\t/");
    }

    #[test]
    fn deep_dumps_parse_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let depth = 50_000;
                let mut dump = String::from(r#"[1,2,{},[{"name":"/r"}"#);
                for _ in 0..depth {
                    dump.push_str(r#",[{"name":"d","dsize":512}"#);
                }
                dump.push_str(&"]".repeat(depth + 2));
                let tree = import(&dump, false);
                assert_eq!(tree.root.size, 512 * depth as u64);
                assert_eq!(tree.stats.total_dirs, depth as u64 + 1);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn malformed_dumps_are_errors_not_panics() {
        for dump in [
            "",
            "[",
            "[2,0,{},[{\"name\":\"/r\"}]]",
            "[1,2,{},[{\"name\":\"/r\"}]",
            "[1,2,{},[{\"name\":\"/r\"},{\"asize\":1}]]",
            "[1,2,{},[{\"name\":\"/r\"},{\"name\":\"x\",\"asize\":-1}]]",
            "[1,2,{},[{\"name\":\"/r\"},{\"name\":\"x\",\"asize\":1e400}]]",
            "[1,2,{},[{\"name\":\"/r\"},{\"name\":\"\\u12\"}]]",
            "[1,2,{},[{\"name\":\"/r\"}]] trailing",
            "[1,2,{},[{}]]",
        ] {
            assert!(
                read(dump.as_bytes(), &ImportOptions::default()).is_err(),
                "{dump:?}"
            );
        }
        let limited = ImportOptions {
            max_entries: Some(2),
            ..ImportOptions::default()
        };
        assert!(matches!(
            read(SAMPLE.as_bytes(), &limited),
            Err(SnapshotError::EntryLimitExceeded { limit: 2 })
        ));
    }

    /// Found by fuzzing: a time past what `SystemTime` holds panicked.
    #[test]
    fn times_out_of_range_are_unknown() {
        let max = u64::MAX;
        let dump = format!(
            r#"[1,2,{{"timestamp":{max}}},[{{"name":"/r"}},{{"name":"x","mtime":{max}}}]]"#
        );
        let tree = import(&dump, true);
        assert_eq!(tree.root.children[0].timestamps.modified, UNIX_EPOCH);
    }
}
