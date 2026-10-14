//! Format decoders. Each writes into a fresh, private staging directory and
//! enforces path containment and [`ExtractLimits`] on bytes actually written.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::{
    ArchiveError, ArchiveFormat, ArchiveInput, ArchiveResult, ExtractLimits,
    MAX_SYMLINK_TARGET_BYTES, MAX_TAR_ENTRY_METADATA_READ_BYTES, MAX_TAR_METADATA_READ_BYTES,
    MAX_XZ_DECODER_MEMORY, MAX_ZIP_METADATA_READ_BYTES, check_cancel, invalid_data, parser_budget,
};

/// Decode `input` into the empty directory `destination`, returning every
/// path written.
pub(super) fn decode(
    format: ArchiveFormat,
    input: File,
    destination: &Path,
    limits: &ExtractLimits,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let tar = |reader: Box<dyn Read>| {
        extract_complete_tar(ArchiveInput::new(reader, cancel), destination, limits)
    };
    match format {
        ArchiveFormat::Zip => extract_zip_with_metadata_budget(
            input,
            destination,
            MAX_ZIP_METADATA_READ_BYTES,
            limits,
            cancel,
        ),
        ArchiveFormat::Tar => tar(Box::new(input)),
        ArchiveFormat::TarGz => tar(Box::new(flate2::read::MultiGzDecoder::new(input))),
        ArchiveFormat::TarBz2 => tar(Box::new(bzip2::read::MultiBzDecoder::new(input))),
        ArchiveFormat::TarXz => {
            let stream = xz2::stream::Stream::new_stream_decoder(
                MAX_XZ_DECODER_MEMORY,
                xz2::stream::CONCATENATED,
            )
            .map_err(std::io::Error::other)?;
            tar(Box::new(xz2::read::XzDecoder::new_stream(input, stream)))
        }
        ArchiveFormat::TarZst => {
            let mut decoder = zstd::stream::read::Decoder::new(input)?;
            // 2^31-byte windows bound decoder memory for hostile frames.
            decoder.window_log_max(31)?;
            tar(Box::new(decoder))
        }
    }
}

fn path_error(message: &str, path: &Path) -> ArchiveError {
    invalid_data(format!("{message}: {}", path.display()))
}

/// Reject absolute paths, paths with a root or drive (`/etc` is not absolute
/// on Windows, but still escapes) and `..` components.
pub(super) fn validate_entry_path(path: &Path) -> ArchiveResult<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(path_error("Absolute path in archive", path));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(path_error("Path traversal attempt in archive", path));
    }
    Ok(())
}

/// Create `outpath`'s parent and require that it resolves inside `root`,
/// so an earlier symlink entry cannot redirect later writes.
fn prepare_parent(outpath: &Path, root: &Path, entry: &Path) -> ArchiveResult<()> {
    if let Some(parent) = outpath.parent() {
        fs::create_dir_all(parent)?;
        if !dunce::canonicalize(parent)?.starts_with(root) {
            return Err(path_error("Path escapes destination", entry));
        }
    }
    Ok(())
}

/// Copy while enforcing per-entry and cumulative limits on bytes written,
/// since declared sizes are attacker-controlled.
pub(super) fn copy_with_limit<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    already_extracted: u64,
    entry_name: &str,
    limits: &ExtractLimits,
) -> ArchiveResult<u64> {
    let mut buffer = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(written);
        }
        written = written.saturating_add(count as u64);
        if written > limits.max_entry_bytes {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{entry_name}' extracted past the per-entry limit of {} bytes",
                limits.max_entry_bytes
            )));
        }
        if already_extracted.saturating_add(written) > limits.max_total_bytes {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Archive exceeded the total extraction limit of {} bytes",
                limits.max_total_bytes
            )));
        }
        writer.write_all(&buffer[..count])?;
    }
}

pub(super) fn extract_zip_with_metadata_budget(
    file: File,
    destination: &Path,
    metadata_read_bytes: u64,
    limits: &ExtractLimits,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let budget = parser_budget::MetadataBudget::new(metadata_read_bytes);
    let parsed = zip::ZipArchive::new(budget.wrap(ArchiveInput::new(file, cancel)));
    // The parser may retry and report an earlier error; keep the budget diagnosis.
    if budget.exhausted() {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP metadata parsing exceeds read budget of {metadata_read_bytes} bytes"
        )));
    }
    let mut archive = parsed?;
    budget.finish_metadata();
    let root = dunce::canonicalize(destination)?;

    if archive.len() as u64 > limits.max_entries {
        return Err(ArchiveError::DecompressionBomb(format!(
            "Archive exceeds maximum entry count of {}",
            limits.max_entries
        )));
    }
    let mut declared_total = 0;
    for index in 0..archive.len() {
        check_cancel(cancel)?;
        let entry = archive.by_index_raw(index)?;
        declared_total = validate_zip_sizes(
            entry.compressed_size(),
            entry.size(),
            declared_total,
            entry.name(),
            limits,
        )?;
    }

    let mut extracted = Vec::new();
    let mut written_total: u64 = 0;
    // Symlinks go last so none can redirect a later write.
    let mut symlinks = Vec::new();
    for index in 0..archive.len() {
        check_cancel(cancel)?;
        let mut entry = archive.by_index(index)?;
        let entry_path = entry
            .enclosed_name()
            .ok_or_else(|| invalid_data("Invalid file path in archive"))?;
        validate_entry_path(&entry_path)?;
        if skip_archive_root(&entry_path, entry.is_dir())? {
            continue;
        }
        if entry.is_symlink() {
            symlinks.push(index);
            continue;
        }
        let outpath = destination.join(&entry_path);
        prepare_parent(&outpath, &root, &entry_path)?;
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            let mut output = File::create(&outpath)?;
            written_total = written_total.saturating_add(copy_with_limit(
                &mut ArchiveInput::new(&mut entry, cancel),
                &mut output,
                written_total,
                &entry_path.display().to_string(),
                limits,
            )?);
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                // Never restore setuid/setgid/sticky bits.
                output.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
        extracted.push(outpath);
    }

    for index in symlinks {
        check_cancel(cancel)?;
        let mut entry = archive.by_index(index)?;
        let entry_path = entry
            .enclosed_name()
            .ok_or_else(|| invalid_data("Invalid file path in archive"))?;
        let outpath = destination.join(&entry_path);
        prepare_parent(&outpath, &root, &entry_path)?;
        let mut target = Vec::new();
        let read = (&mut entry)
            .take(MAX_SYMLINK_TARGET_BYTES + 1)
            .read_to_end(&mut target)?;
        if read as u64 > MAX_SYMLINK_TARGET_BYTES {
            return Err(path_error("Symlink target too long", &entry_path));
        }
        let target = zip_symlink_target(&target)?;
        validate_symlink_target(target, &outpath, &root)?;
        create_symlink(target, &outpath)?;
        extracted.push(outpath);
    }
    Ok(extracted)
}

pub(super) fn zip_symlink_target(bytes: &[u8]) -> std::io::Result<&Path> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ZIP symlink target is not UTF-8",
        )
    })?;
    if text.is_empty() || text.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ZIP symlink target is empty or contains NUL",
        ));
    }
    // Whitespace is part of a filename, not padding.
    Ok(Path::new(text))
}

/// Check declared sizes before invoking a ZIP entry decoder.
pub(super) fn validate_zip_sizes(
    compressed: u64,
    uncompressed: u64,
    total: u64,
    name: &str,
    limits: &ExtractLimits,
) -> ArchiveResult<u64> {
    if uncompressed > limits.max_entry_bytes
        || uncompressed > limits.max_total_bytes.saturating_sub(total)
    {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' exceeds the per-entry or remaining extraction budget"
        )));
    }
    if compressed == 0 && uncompressed > 0 {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' declares nonempty output with zero compressed bytes"
        )));
    }
    if compressed > 0 && uncompressed as f64 / compressed as f64 > limits.max_ratio {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' exceeds the maximum decompression ratio of {}:1",
            limits.max_ratio
        )));
    }
    Ok(total + uncompressed)
}

/// Decode a TAR stream, then drain the decoder so compression checksums and
/// trailers are verified. Bounded zero padding is allowed; data is not.
pub(super) fn extract_complete_tar<R: Read>(
    mut reader: R,
    destination: &Path,
    limits: &ExtractLimits,
) -> ArchiveResult<Vec<PathBuf>> {
    let extracted = extract_tar_from_reader(&mut reader, destination, limits)?;
    const MAX_PADDING: usize = 1024 * 1024;
    let mut padding = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(extracted);
        }
        padding += count;
        if padding > MAX_PADDING {
            return Err(ArchiveError::DecompressionBomb(
                "TAR trailing padding exceeds 1 MiB".into(),
            ));
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(invalid_data("Nonzero data follows the TAR end marker"));
        }
    }
}

/// Lexically normalize `..` and `.`, preserving root and prefix components.
pub(super) fn resolve_path(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// First-line symlink check at decode time: no absolute targets and no
/// lexical escape. The kernel-accurate check runs after decoding.
fn validate_symlink_target(target: &Path, outpath: &Path, root: &Path) -> ArchiveResult<()> {
    if target.is_absolute()
        || target
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(path_error("Symlink with absolute target rejected", target));
    }
    let parent = outpath.parent().unwrap_or(root);
    let parent = dunce::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    if !resolve_path(&parent.join(target)).starts_with(root) {
        return Err(path_error("Symlink escapes destination", target));
    }
    Ok(())
}

/// TAR hard links name archive-root-relative regular files already extracted.
pub(super) fn validate_tar_hardlink_target(target: &Path, root: &Path) -> ArchiveResult<()> {
    if target.is_absolute()
        || target
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid_data(
            "TAR hard-link target must be relative to the archive root without parent traversal",
        ));
    }
    let path = root.join(target);
    if !fs::symlink_metadata(&path)?.is_file() || !dunce::canonicalize(&path)?.starts_with(root) {
        return Err(invalid_data(
            "TAR hard-link target must be an extracted regular file inside the archive root",
        ));
    }
    Ok(())
}

/// Root-directory entries (`./`) belong to the host and are skipped.
pub(super) fn skip_archive_root(path: &Path, is_directory: bool) -> ArchiveResult<bool> {
    let is_root = path
        .components()
        .all(|component| matches!(component, Component::CurDir));
    if is_root && !is_directory {
        return Err(invalid_data(
            "Archive entry resolves to the extraction root but is not a directory",
        ));
    }
    Ok(is_root)
}

pub(super) fn extract_tar_from_reader<R: Read>(
    reader: R,
    destination: &Path,
    limits: &ExtractLimits,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_tar_with_metadata_budget(
        reader,
        destination,
        MAX_TAR_METADATA_READ_BYTES,
        MAX_TAR_ENTRY_METADATA_READ_BYTES,
        limits,
    )
}

pub(super) fn extract_tar_with_metadata_budget<R: Read>(
    reader: R,
    destination: &Path,
    total_budget: u64,
    entry_budget: u64,
    limits: &ExtractLimits,
) -> ArchiveResult<Vec<PathBuf>> {
    let budget = parser_budget::MetadataBudget::new(total_budget);
    let mut archive = tar::Archive::new(budget.wrap(reader));
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    let root = dunce::canonicalize(destination)?;

    let mut extracted = Vec::new();
    // Directory modes are applied after every entry is written, so a
    // read-only directory never blocks its own contents.
    let mut directory_modes: Vec<(PathBuf, u32)> = Vec::new();
    let mut entry_count: u64 = 0;
    let mut written_total: u64 = 0;

    let mut entries = archive.entries()?;
    loop {
        budget.resume_metadata(entry_budget);
        let next = entries.next();
        if budget.exhausted() {
            return Err(ArchiveError::DecompressionBomb(format!(
                "TAR metadata exceeds cumulative {total_budget}-byte or per-entry {entry_budget}-byte read budget"
            )));
        }
        budget.finish_metadata();
        let Some(entry) = next else { break };
        let mut entry = entry?;

        entry_count += 1;
        if entry_count > limits.max_entries {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Archive exceeds maximum entry count of {}",
                limits.max_entries
            )));
        }

        let entry_path = entry.path()?.into_owned();
        validate_entry_path(&entry_path)?;
        let entry_type = entry.header().entry_type();
        if skip_archive_root(&entry_path, entry_type.is_dir())? {
            continue;
        }
        if entry_type.is_fifo()
            || entry_type.is_character_special()
            || entry_type.is_block_special()
        {
            return Err(path_error(
                "Unsupported special-file TAR entry",
                &entry_path,
            ));
        }

        // The effective size includes sparse extents and PAX overrides.
        let declared = entry.size();
        if declared > limits.max_entry_bytes
            || declared > limits.max_total_bytes.saturating_sub(written_total)
        {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' effective size {declared} exceeds the per-entry or remaining extraction budget",
                entry_path.display(),
            )));
        }

        let outpath = destination.join(&entry_path);
        prepare_parent(&outpath, &root, &entry_path)?;

        if entry_type.is_dir() {
            fs::create_dir_all(&outpath)?;
            if !dunce::canonicalize(&outpath)?.starts_with(&root) {
                return Err(path_error("Path escapes destination", &entry_path));
            }
            directory_modes.push((
                outpath.clone(),
                entry.header().mode().unwrap_or(0o755) & 0o777,
            ));
            extracted.push(outpath);
            continue;
        }

        if entry_type.is_symlink() || entry_type.is_hard_link() {
            let Ok(Some(target)) = entry.link_name() else {
                return Err(path_error(
                    "Link entry has an unreadable or missing target",
                    &entry_path,
                ));
            };
            if entry_type.is_hard_link() {
                validate_tar_hardlink_target(&target, &root)?;
            } else {
                validate_symlink_target(&target, &outpath, &root)?;
            }
        }

        // `unpack_in` resolves hard links against the archive root and
        // re-validates containment.
        if !entry.unpack_in(destination)? {
            return Err(path_error(
                "TAR entry could not be safely unpacked",
                &entry_path,
            ));
        }
        // Count bytes actually written: declared sizes are untrusted.
        let actual = fs::symlink_metadata(&outpath).map(|m| m.len()).unwrap_or(0);
        written_total = written_total.saturating_add(actual);
        if actual > limits.max_entry_bytes || written_total > limits.max_total_bytes {
            let _ = fs::remove_file(&outpath);
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' pushed extraction past its size limits",
                entry_path.display()
            )));
        }
        extracted.push(outpath);
    }

    #[cfg(unix)]
    for (directory, mode) in directory_modes.into_iter().rev() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    drop(directory_modes);
    Ok(extracted)
}

fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        // Resolve relative to the link's directory to choose the link kind.
        let resolved = link.parent().unwrap_or(Path::new("")).join(target);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
}
