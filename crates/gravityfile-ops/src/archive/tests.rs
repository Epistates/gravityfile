use std::io::Write;

use super::create::*;
use super::extract::*;
use super::*;
use tempfile::TempDir;

const GIB: u64 = 1024 * 1024 * 1024;

/// Fixed limits so boundary assertions do not drift with the defaults.
fn limits() -> ExtractLimits {
    ExtractLimits {
        max_total_bytes: 10 * GIB,
        max_entry_bytes: 10 * GIB,
        max_entries: 100_000,
        max_ratio: 1000.0,
    }
}

fn extract_zip(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_zip_with_metadata_budget(
        file,
        destination,
        MAX_ZIP_METADATA_READ_BYTES,
        &limits(),
        cancel,
    )
}

fn extract_tar(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_complete_tar(ArchiveInput::new(file, cancel), destination, &limits())
}

#[test]
fn tar_extension_reads_stop_before_advertised_gigabyte_body() {
    use std::cell::Cell;
    use std::io::{Cursor, Read};
    use std::rc::Rc;
    struct Counted<R>(R, Rc<Cell<u64>>);
    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let read = self.0.read(bytes)?;
            self.1.set(self.1.get() + read as u64);
            Ok(read)
        }
    }
    for kind in *b"LKx" {
        let directory = tempfile::tempdir().unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_path("extension").unwrap();
        header.set_entry_type(tar::EntryType::new(kind));
        header.set_size(1 << 30);
        header.set_cksum();
        let reads = Rc::new(Cell::new(0));
        let reader = Counted(
            Cursor::new(header.as_bytes().to_vec()).chain(std::io::repeat(0).take(1 << 30)),
            reads.clone(),
        );
        let error =
            extract_tar_with_metadata_budget(reader, directory.path(), 8192, 4096, &limits())
                .unwrap_err();
        assert!(
            matches!(error, super::ArchiveError::DecompressionBomb(_)),
            "{error}"
        );
        assert_eq!(reads.get(), 4096);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn tar_metadata_budget_accumulates_but_excludes_regular_payload() {
    let directory = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(8192);
    header.set_mode(0o600);
    builder
        .append_data(&mut header, "payload", &[42; 8192][..])
        .unwrap();
    let bytes = builder.into_inner().unwrap();
    extract_tar_with_metadata_budget(bytes.as_slice(), directory.path(), 1536, 1024, &limits())
        .unwrap();
    assert_eq!(
        std::fs::read(directory.path().join("payload")).unwrap(),
        vec![42; 8192]
    );

    let mut builder = tar::Builder::new(Vec::new());
    for i in 0..4 {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o600);
        builder
            .append_data(&mut header, format!("empty-{i}"), std::io::empty())
            .unwrap();
    }
    let bytes = builder.into_inner().unwrap();
    let error =
        extract_tar_with_metadata_budget(bytes.as_slice(), directory.path(), 1536, 1024, &limits())
            .unwrap_err();
    assert!(matches!(error, super::ArchiveError::DecompressionBomb(_)));
    assert!(!directory.path().join("empty-3").exists());
}

#[test]
fn oversized_compressed_tar_metadata_never_publishes_destination() {
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("bomb.tar.gz");
    let destination = directory.path().join("output");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&archive).unwrap(),
        flate2::Compression::fast(),
    );
    let mut header = tar::Header::new_gnu();
    header.set_path("extension").unwrap();
    header.set_entry_type(tar::EntryType::new(b'L'));
    header.set_size(2 * super::MAX_TAR_ENTRY_METADATA_READ_BYTES);
    header.set_cksum();
    encoder.write_all(header.as_bytes()).unwrap();
    std::io::copy(
        &mut std::io::repeat(0).take(header.size().unwrap()),
        &mut encoder,
    )
    .unwrap();
    encoder.finish().unwrap();
    let error = super::extract_archive(&archive, &destination).unwrap_err();
    assert!(
        matches!(error, super::ArchiveError::DecompressionBomb(_)),
        "{error}"
    );
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn zip_metadata_budget_rejects_before_materialization_but_not_payload_reads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.zip");
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..32 {
        archive
            .start_file(format!("file-{index:02}"), options)
            .unwrap();
        archive.write_all(&vec![b'x'; 64 * 1024]).unwrap();
    }
    std::fs::write(&path, archive.finish().unwrap().into_inner()).unwrap();
    let staging = tempfile::tempdir().unwrap();
    let error = extract_zip_with_metadata_budget(
        open_archive_input(&path).unwrap(),
        staging.path(),
        1024,
        &limits(),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ArchiveError::DecompressionBomb(_)),
        "{error}"
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    // The same archive fits a larger metadata budget. Its 2 MiB payload is
    // deliberately much larger than that budget, and must still extract.
    extract_zip_with_metadata_budget(
        open_archive_input(&path).unwrap(),
        staging.path(),
        64 * 1024,
        &limits(),
        &CancellationToken::new(),
    )
    .unwrap();
    for index in 0..32 {
        assert_eq!(
            std::fs::read(staging.path().join(format!("file-{index:02}"))).unwrap(),
            vec![b'x'; 64 * 1024]
        );
    }
}

#[test]
fn zip_size_preflight_checks_individual_total_and_zero_input_limits() {
    let limits = limits();
    let check = |compressed, uncompressed, total, name| {
        validate_zip_sizes(compressed, uncompressed, total, name, &limits)
    };
    let max = limits.max_total_bytes;
    assert_eq!(check(0, 0, 0, "empty").unwrap(), 0);
    assert!(check(0, 1, 0, "impossible").is_err());
    let huge = limits.max_entry_bytes + 1;
    assert!(check(huge, huge, 0, "huge").is_err());
    assert!(check(2, 2, max - 1, "over-total").is_err());
    assert_eq!(check(1, 1, max - 1, "boundary").unwrap(), max);
    assert!(check(1, 1001, 0, "ratio").is_err());
    assert_eq!(check(1, 1000, 0, "ratio-boundary").unwrap(), 1000);
}

#[test]
fn zip_zero_compressed_nonempty_entry_is_rejected_before_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.zip");
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file(
            "payload",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    archive.write_all(b"content").unwrap();
    let mut bytes = archive.finish().unwrap().into_inner();
    let central = bytes
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    // Central-directory compressed-size field; preserve nonempty output size.
    bytes[central + 20..central + 24].copy_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let staging = tempfile::tempdir().unwrap();
    let error = extract_zip(
        open_archive_input(&path).unwrap(),
        staging.path(),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ArchiveError::DecompressionBomb(_)),
        "{error}"
    );
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[test]
fn xz_dictionary_memory_request_is_rejected_before_publication() {
    // XZ stream + block headers with valid CRC32s. The LZMA2 property 0x28
    // requests a 4 GiB dictionary. No compressed payload is needed to reject
    // the header, and this fixture never constructs a large encoder.
    let header = [
        253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 2, 0, 33, 1, 40, 0, 0, 0, 230, 160, 17,
        179,
    ];
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("large-dictionary.tar.xz");
    let destination = directory.path().join("output");
    std::fs::write(&archive, header).unwrap();
    let error = extract_archive(&archive, &destination).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("memory"),
        "{error}"
    );
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

fn encode_test_member(format: ArchiveFormat, bytes: &[u8]) -> Vec<u8> {
    match format {
        ArchiveFormat::TarGz => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        ArchiveFormat::TarBz2 => {
            let mut encoder =
                bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        ArchiveFormat::TarXz => {
            let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        ArchiveFormat::TarZst => zstd::encode_all(bytes, 1).unwrap(),
        _ => panic!("compressed TAR format required"),
    }
}

#[test]
fn concatenated_members_can_split_tar_headers_payloads_and_padding() {
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(1024);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, "payload", [b'x'; 1024].as_slice())
        .unwrap();
    let bytes = tar.into_inner().unwrap();
    for format in [
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        for split in [13, 600, bytes.len() - 100] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("input{}", format.extension()));
            let mut encoded = encode_test_member(format, &bytes[..split]);
            encoded.extend_from_slice(&encode_test_member(format, &bytes[split..]));
            std::fs::write(&path, encoded).unwrap();
            let destination = directory.path().join("output");
            extract_archive(&path, &destination).unwrap();
            assert_eq!(
                std::fs::read(destination.join("payload")).unwrap(),
                [b'x'; 1024]
            );
        }
    }
}

#[test]
fn later_compression_members_cannot_hide_data_or_truncated_trailers() {
    for format in [
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        for truncated in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("input{}", format.extension()));
            let mut encoded = encode_test_member(format, &[0; 1024]);
            let mut second =
                encode_test_member(format, if truncated { &[0; 512] } else { b"hidden data" });
            if truncated {
                second.truncate(second.len() - 4);
            }
            encoded.extend_from_slice(&second);
            std::fs::write(&path, encoded).unwrap();
            let destination = directory.path().join("output");
            assert!(
                extract_archive(&path, &destination).is_err(),
                "{format:?}, truncated={truncated}"
            );
            assert!(!destination.exists());
        }
    }
}

#[test]
fn truncated_compression_trailers_fail_before_publication() {
    for format in [
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        let archive_path = directory
            .path()
            .join(format!("archive{}", format.extension()));
        create_archive(&[source], &archive_path, format).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&archive_path)
            .unwrap();
        let length = file.metadata().unwrap().len();
        file.set_len(length - 4).unwrap();
        drop(file);
        let destination = directory.path().join("destination");
        assert!(
            extract_archive(&archive_path, &destination).is_err(),
            "{format:?}"
        );
        assert!(!destination.exists(), "{format:?}");
    }
}

#[test]
fn corrupt_gzip_checksum_fails_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    std::fs::write(&source, b"content").unwrap();
    let archive_path = directory.path().join("archive.tar.gz");
    create_archive(&[source], &archive_path, ArchiveFormat::TarGz).unwrap();
    let mut bytes = std::fs::read(&archive_path).unwrap();
    let crc_start = bytes.len() - 8;
    bytes[crc_start] ^= 1;
    std::fs::write(&archive_path, bytes).unwrap();
    let destination = directory.path().join("destination");
    assert!(extract_archive(&archive_path, &destination).is_err());
    assert!(!destination.exists());
}

#[test]
fn tar_trailing_padding_is_bounded_and_cannot_hide_data() {
    let directory = tempfile::tempdir().unwrap();
    let mut valid = vec![0u8; 1024];
    valid.extend_from_slice(&[0; 8192]);
    assert!(
        extract_complete_tar(valid.as_slice(), directory.path(), &limits())
            .unwrap()
            .is_empty()
    );
    let mut nonzero = vec![0u8; 1024];
    nonzero.extend_from_slice(b"hidden content");
    assert!(
        extract_complete_tar(nonzero.as_slice(), directory.path(), &limits())
            .unwrap_err()
            .to_string()
            .contains("Nonzero data")
    );
    let oversized = vec![0u8; 1024 * 1024 + 1024];
    assert!(matches!(
        extract_complete_tar(oversized.as_slice(), directory.path(), &limits()),
        Err(ArchiveError::DecompressionBomb(_))
    ));
}

#[test]
fn archive_input_errors_do_not_create_destination_parents() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("new-parent/output");
    let missing = directory.path().join("missing.zip");
    assert!(matches!(
        extract_archive(&missing, &destination),
        Err(ArchiveError::NotFound(_))
    ));
    let input_directory = directory.path().join("directory.zip");
    std::fs::create_dir(&input_directory).unwrap();
    assert!(matches!(
        extract_archive(&input_directory, &destination),
        Err(ArchiveError::Io(_))
    ));
    assert!(!directory.path().join("new-parent").exists());
}

#[cfg(unix)]
#[test]
fn archive_input_rejects_fifo_without_waiting_for_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("input.tar");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path throughout the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = extract_archive(&fifo, &directory.path().join("out")).unwrap_err();
    assert!(error.to_string().contains("regular file"));
    assert!(!directory.path().join("out").exists());
}

#[cfg(unix)]
#[test]
fn archive_input_accepts_regular_symlink_and_keeps_opened_identity() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.tar");
    let alias = directory.path().join("alias.tar");
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o600);
    header.set_cksum();
    archive
        .append_data(&mut header, "payload", b"content".as_slice())
        .unwrap();
    std::fs::write(&input, archive.into_inner().unwrap()).unwrap();
    symlink(&input, &alias).unwrap();
    let file = open_archive_input(&alias).unwrap();
    std::fs::rename(&input, directory.path().join("original.tar")).unwrap();
    std::fs::write(&input, b"replacement is not an archive").unwrap();
    let staging = tempfile::tempdir().unwrap();
    extract_tar(file, staging.path(), &CancellationToken::new()).unwrap();
    assert_eq!(
        std::fs::read(staging.path().join("payload")).unwrap(),
        b"content"
    );
}

fn sparse_test_archive(logical_size: u64) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::GNUSparse);
    header.set_mode(0o600);
    header.set_size(1);
    let gnu = header.as_gnu_mut().unwrap();
    gnu.set_real_size(logical_size);
    gnu.sparse[0].set_offset(logical_size - 1);
    gnu.sparse[0].set_length(1);
    header.set_cksum();
    archive
        .append_data(&mut header, "nested/sparse", b"x".as_slice())
        .unwrap();
    archive.into_inner().unwrap()
}

#[test]
fn oversized_sparse_tar_is_rejected_before_creating_output_paths() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = sparse_test_archive(limits().max_entry_bytes + 1);
    let error = extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).unwrap_err();
    assert!(matches!(error, ArchiveError::DecompressionBomb(_)));
    assert!(!directory.path().join("nested").exists());
}

#[test]
fn bounded_sparse_tar_retains_its_logical_contents() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = sparse_test_archive(64);
    extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).unwrap();
    let content = std::fs::read(directory.path().join("nested/sparse")).unwrap();
    assert_eq!(content.len(), 64);
    assert!(content[..63].iter().all(|byte| *byte == 0));
    assert_eq!(content[63], b'x');
}

#[test]
fn tar_special_file_headers_are_rejected_before_materialization() {
    for kind in [
        tar::EntryType::Fifo,
        tar::EntryType::Char,
        tar::EntryType::Block,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(0o600);
        header.set_size(0);
        header.set_cksum();
        archive
            .append_data(&mut header, "nested/special", std::io::empty())
            .unwrap();
        let bytes = archive.into_inner().unwrap();
        let error =
            extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).unwrap_err();
        assert!(error.to_string().contains("Unsupported special-file"));
        assert!(!directory.path().join("nested").exists());
    }
}

fn append_test_hardlink(archive: &mut tar::Builder<Vec<u8>>, name: &str, target: &str) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Link);
    header.set_mode(0o644);
    header.set_size(0);
    header.set_link_name(target).unwrap();
    header.set_cksum();
    archive
        .append_data(&mut header, name, std::io::empty())
        .unwrap();
}

#[test]
fn tar_hardlinks_resolve_from_archive_root_including_nested_entries() {
    let directory = tempfile::tempdir().unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(7);
    header.set_cksum();
    archive
        .append_data(&mut header, "payload", b"content".as_slice())
        .unwrap();
    append_test_hardlink(&mut archive, "nested/link", "payload");
    let bytes = archive.into_inner().unwrap();
    extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).unwrap();
    assert_eq!(
        std::fs::read(directory.path().join("nested/link")).unwrap(),
        b"content"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(directory.path().join("payload"))
                .unwrap()
                .ino(),
            std::fs::metadata(directory.path().join("nested/link"))
                .unwrap()
                .ino()
        );
    }
}

#[test]
fn tar_hardlinks_cannot_import_files_from_process_working_directory() {
    // Cargo runs this test from a package directory that contains Cargo.toml.
    assert!(Path::new("Cargo.toml").is_file());
    let directory = tempfile::tempdir().unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    append_test_hardlink(&mut archive, "imported", "Cargo.toml");
    let bytes = archive.into_inner().unwrap();
    assert!(extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).is_err());
    assert!(!directory.path().join("imported").exists());
}

#[cfg(unix)]
#[test]
fn tar_hardlink_validation_rejects_symlinks_and_parent_traversal() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(root.join("payload"), b"content").unwrap();
    symlink("payload", root.join("link")).unwrap();
    for target in [Path::new("link"), Path::new("../payload"), Path::new(".")] {
        assert!(validate_tar_hardlink_target(target, &root).is_err());
    }
}

#[cfg(unix)]
#[test]
fn tar_root_headers_cannot_change_private_staging_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let parent = tempfile::tempdir().unwrap();
    let staging = crate::fs::staging_dir(&parent.path().join("x"), ".extract-test-").unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    for mode in [0o777, 0o000] {
        let mut root = tar::Header::new_gnu();
        root.set_entry_type(tar::EntryType::Directory);
        root.set_mode(mode);
        root.set_size(0);
        root.set_cksum();
        archive
            .append_data(&mut root, ".", std::io::empty())
            .unwrap();
    }
    let mut file = tar::Header::new_gnu();
    file.set_mode(0o644);
    file.set_size(7);
    file.set_cksum();
    archive
        .append_data(&mut file, "payload", b"content".as_slice())
        .unwrap();
    let bytes = archive.into_inner().unwrap();
    let extracted = extract_tar_from_reader(bytes.as_slice(), staging.path(), &limits()).unwrap();
    assert_eq!(extracted, vec![staging.path().join("payload")]);
    assert_eq!(
        std::fs::metadata(staging.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read(staging.path().join("payload")).unwrap(),
        b"content"
    );
}

#[test]
fn tar_rejects_regular_file_at_extraction_root() {
    let directory = tempfile::tempdir().unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    let mut root = tar::Header::new_gnu();
    root.set_mode(0o644);
    root.set_size(0);
    root.set_cksum();
    archive
        .append_data(&mut root, ".", std::io::empty())
        .unwrap();
    let bytes = archive.into_inner().unwrap();
    let error = extract_tar_from_reader(bytes.as_slice(), directory.path(), &limits()).unwrap_err();
    assert!(error.to_string().contains("extraction root"));
    assert!(directory.path().is_dir());
}

#[test]
fn zip_root_directory_is_skipped_without_losing_contents() {
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("root.zip");
    let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    archive.add_directory("./", options).unwrap();
    archive.start_file("payload", options).unwrap();
    archive.write_all(b"content").unwrap();
    archive.finish().unwrap();
    let staging = crate::fs::staging_dir(&directory.path().join("x"), ".extract-test-").unwrap();
    let extracted = extract_zip(
        open_archive_input(&archive_path).unwrap(),
        staging.path(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(extracted, vec![staging.path().join("payload")]);
    assert_eq!(
        std::fs::read(staging.path().join("payload")).unwrap(),
        b"content"
    );
}

#[test]
fn colliding_source_names_are_rejected_before_publication() {
    for format in [
        ArchiveFormat::Zip,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let root = TempDir::new().unwrap();
        let mut sources = Vec::new();
        for parent in ["one", "two"] {
            let dir = root.path().join(parent);
            std::fs::create_dir(&dir).unwrap();
            let file = dir.join("same.txt");
            std::fs::write(&file, parent).unwrap();
            sources.push(file);
        }
        let destination = root.path().join("output");
        assert!(create_archive(&sources, &destination, format).is_err());
        assert!(!destination.exists());
    }
}

#[cfg(unix)]
#[test]
fn zip_symlinks_round_trip_as_links() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("target"), "contents").unwrap();
    std::os::unix::fs::symlink("target", source.join("link")).unwrap();
    let archive = root.path().join("archive.zip");
    create_archive(&[source], &archive, ArchiveFormat::Zip).unwrap();
    let destination = root.path().join("extracted");
    extract_archive(&archive, &destination).unwrap();
    let link = destination.join("source/link");
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("target"));
    assert_eq!(std::fs::read_to_string(link).unwrap(), "contents");
}

#[cfg(target_os = "linux")]
#[test]
fn tar_round_trips_non_utf8_top_level_names_and_symlink_targets() {
    use std::os::unix::ffi::OsStringExt;
    for format in [
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let root = TempDir::new().unwrap();
        let file_name = std::ffi::OsString::from_vec(b"file-\xff".to_vec());
        let directory_name = std::ffi::OsString::from_vec(b"dir-\xfe".to_vec());
        let link_name = std::ffi::OsString::from_vec(b"link-\xfd".to_vec());
        let file = root.path().join(&file_name);
        let directory = root.path().join(&directory_name);
        let link = root.path().join(&link_name);
        std::fs::write(&file, b"file contents").unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(&file_name), b"nested contents").unwrap();
        std::os::unix::fs::symlink(&file_name, &link).unwrap();
        let archive = root.path().join(format!("archive{}", format.extension()));
        create_archive(&[file, directory, link], &archive, format).unwrap();
        let destination = root.path().join("extracted");
        extract_archive(&archive, &destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join(&file_name)).unwrap(),
            b"file contents"
        );
        assert_eq!(
            std::fs::read(destination.join(&directory_name).join(&file_name)).unwrap(),
            b"nested contents"
        );
        assert_eq!(
            std::fs::read_link(destination.join(&link_name)).unwrap(),
            PathBuf::from(&file_name)
        );
        assert_eq!(
            std::fs::read(destination.join(&link_name)).unwrap(),
            b"file contents"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn zip_rejects_lossy_nested_names_and_link_targets() {
    use std::os::unix::ffi::OsStringExt;
    let root = TempDir::new().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let invalid = std::ffi::OsString::from_vec(vec![b'x', 255]);
    std::fs::write(source.join(&invalid), "contents").unwrap();
    let destination = root.path().join("archive.zip");
    assert!(create_archive(&[source], &destination, ArchiveFormat::Zip).is_err());
    assert!(!destination.exists());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&invalid, &link).unwrap();
    assert!(create_archive(&[link], &destination, ArchiveFormat::Zip).is_err());
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn zip_path_encoding_never_replaces_native_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let invalid = std::ffi::OsString::from_vec(vec![b'x', 255]);
    assert!(zip_utf8(&invalid).is_err());
    assert_eq!(zip_utf8(std::ffi::OsStr::new("résumé")).unwrap(), "résumé");
}

#[test]
fn compressed_tar_reports_final_output_failure() {
    struct LimitedWriter {
        remaining: usize,
    }
    impl Write for LimitedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected storage exhaustion"));
            }
            let count = self.remaining.min(bytes.len());
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for format in [
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        // Empty TAR has a real compressed trailer. Fail on the very last
        // output byte, after the encoder has accepted the TAR contents.
        let complete = create_compressed_tar(&[], Vec::new(), format).unwrap();
        let error = create_compressed_tar(
            &[],
            LimitedWriter {
                remaining: complete.len() - 1,
            },
            format,
        )
        .err()
        .expect("final write failure was swallowed");
        assert!(error.to_string().contains("injected storage exhaustion"));
    }
}

#[test]
fn archive_publication_preserves_a_concurrent_destination() {
    let root = TempDir::new().unwrap();
    let staged = root.path().join("payload");
    let destination = root.path().join("report.tar");
    std::fs::write(&staged, "complete archive").unwrap();
    // A second writer appeared after the initial existence check.
    std::fs::write(&destination, "concurrent writer").unwrap();
    assert!(matches!(
        crate::fs::rename_noreplace(&staged, &destination),
        Err(crate::fs::RenameError::DestinationExists)
    ));
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "concurrent writer"
    );
    assert_eq!(
        std::fs::read_to_string(&staged).unwrap(),
        "complete archive"
    );
}

#[test]
fn all_archive_formats_finish_and_round_trip() {
    for format in [
        ArchiveFormat::Zip,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source.txt");
        std::fs::write(&source, "archived contents").unwrap();
        let destination = root.path().join(format!("archive.{}", format.extension()));
        create_archive(&[source], &destination, format).unwrap();
        let extracted = root.path().join("extracted");
        extract_archive(&destination, &extracted).unwrap();
        assert_eq!(
            std::fs::read_to_string(extracted.join("source.txt")).unwrap(),
            "archived contents"
        );
        assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".gravityfile-archive-")
        }));
    }
}

#[test]
fn test_archive_format_detection() {
    assert_eq!(
        ArchiveFormat::from_path(Path::new("test.zip")),
        Some(ArchiveFormat::Zip)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("test.tar.gz")),
        Some(ArchiveFormat::TarGz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("test.tgz")),
        Some(ArchiveFormat::TarGz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("test.tar")),
        Some(ArchiveFormat::Tar)
    );
    assert_eq!(ArchiveFormat::from_path(Path::new("test.txt")), None);
}

#[test]
fn test_create_and_extract_zip() {
    let temp_dir = TempDir::new().unwrap();
    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir(&source_dir).unwrap();

    // Create test files
    std::fs::write(source_dir.join("test.txt"), "Hello, World!").unwrap();
    std::fs::create_dir(source_dir.join("subdir")).unwrap();
    std::fs::write(source_dir.join("subdir/nested.txt"), "Nested content").unwrap();

    // Create archive
    let archive_path = temp_dir.path().join("test.zip");
    create_archive(
        std::slice::from_ref(&source_dir),
        &archive_path,
        ArchiveFormat::Zip,
    )
    .unwrap();

    assert!(archive_path.exists());

    // Extract archive
    let extract_dir = temp_dir.path().join("extracted");
    let extracted = extract_archive(&archive_path, &extract_dir).unwrap();

    assert!(!extracted.is_empty());
}

#[test]
fn extraction_conflict_preserves_existing_files_and_publishes_nothing() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let existing_source = source.join("existing.txt");
    let new_source = source.join("new.txt");
    std::fs::write(&existing_source, "archive version").unwrap();
    std::fs::write(&new_source, "new content").unwrap();
    std::fs::write(destination.join("existing.txt"), "original version").unwrap();
    let archive = temp.path().join("conflict.zip");
    create_archive(&[existing_source, new_source], &archive, ArchiveFormat::Zip).unwrap();

    let error = extract_archive(&archive, &destination).unwrap_err();

    assert!(matches!(error, ArchiveError::Io(_)));
    assert_eq!(
        std::fs::read_to_string(destination.join("existing.txt")).unwrap(),
        "original version"
    );
    assert!(!destination.join("new.txt").exists());
    assert!(
        std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .contains("gravityfile-tmp"))
    );
}

#[test]
fn test_create_and_extract_tar_gz() {
    let temp_dir = TempDir::new().unwrap();
    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir(&source_dir).unwrap();

    // Create test files
    std::fs::write(source_dir.join("test.txt"), "Hello from tar.gz!").unwrap();
    std::fs::create_dir(source_dir.join("subdir")).unwrap();
    std::fs::write(source_dir.join("subdir/nested.txt"), "Nested in tar").unwrap();

    // Create archive
    let archive_path = temp_dir.path().join("test.tar.gz");
    create_archive(
        std::slice::from_ref(&source_dir),
        &archive_path,
        ArchiveFormat::TarGz,
    )
    .unwrap();

    assert!(archive_path.exists());

    // Extract archive
    let extract_dir = temp_dir.path().join("extracted");
    let extracted = extract_archive(&archive_path, &extract_dir).unwrap();

    assert!(!extracted.is_empty());
    // Verify content was extracted correctly
    assert!(extract_dir.join("source").exists() || extract_dir.join("test.txt").exists());
}

#[test]
fn test_path_traversal_prevention_absolute_path() {
    let path = Path::new("/etc/passwd");
    assert!(path.is_absolute());

    let has_parent_dir = path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(path.is_absolute() || has_parent_dir);
}

#[test]
fn test_path_traversal_prevention_parent_dir() {
    let path = Path::new("../../../etc/passwd");
    let has_parent_dir = path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(has_parent_dir);

    let path2 = Path::new("foo/../../../bar");
    let has_parent_dir2 = path2
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(has_parent_dir2);

    let safe_path = Path::new("foo/bar/baz.txt");
    let has_parent_dir3 = safe_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(!has_parent_dir3);
}

#[test]
fn test_empty_archive_handling() {
    let temp_dir = TempDir::new().unwrap();

    // Create empty zip
    let archive_path = temp_dir.path().join("empty.zip");
    create_archive(&[], &archive_path, ArchiveFormat::Zip).unwrap();

    // Extract should succeed with empty result
    let extract_dir = temp_dir.path().join("extracted");
    let extracted = extract_archive(&archive_path, &extract_dir).unwrap();
    assert!(extracted.is_empty());
}

#[test]
fn test_archive_destination_not_exists() {
    let temp_dir = TempDir::new().unwrap();

    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test content").unwrap();

    let archive_path = temp_dir.path().join("test.zip");
    create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

    // Extract to non-existent directory (should create it)
    let extract_dir = temp_dir.path().join("new_dir/nested/deep");
    let extracted = extract_archive(&archive_path, &extract_dir).unwrap();
    assert!(!extracted.is_empty());
    assert!(extract_dir.exists());
}

#[test]
fn zip_source_copy_rejects_growth_and_truncation_during_read() {
    struct MutatingWriter {
        source: File,
        append: bool,
        mutated: bool,
        bytes: Vec<u8>,
    }
    impl Write for MutatingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.mutated {
                self.mutated = true;
                if self.append {
                    self.source.write_all(&[2; 1024])?;
                } else {
                    self.source.set_len(0)?;
                }
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for append in [true, false] {
        let root = TempDir::new().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, [1; 1024]).unwrap();
        let inspected = std::fs::symlink_metadata(&path).unwrap();
        let mut source = open_zip_source(&path, &inspected).unwrap();
        let mut writer = MutatingWriter {
            source: std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap(),
            append,
            mutated: false,
            bytes: Vec::new(),
        };
        let error = copy_zip_source(&mut source, &inspected, &mut writer).unwrap_err();
        assert!(error.to_string().contains("ZIP source changed"), "{error}");
        assert!(writer.bytes.len() <= inspected.len() as usize);
    }
}

#[cfg(unix)]
#[test]
fn zip_source_open_rejects_replacement_after_inspection() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("source");
    let retained = root.path().join("retained");
    std::fs::write(&path, b"original").unwrap();
    let inspected = std::fs::symlink_metadata(&path).unwrap();
    let mut file = open_zip_source(&path, &inspected).unwrap();
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();
    assert_eq!(contents, b"original");
    drop(file);
    std::fs::rename(&path, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &path).unwrap();
    assert!(
        open_zip_source(&path, &inspected).is_err(),
        "must not follow a replacement symlink"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(
        open_zip_source(&path, &inspected).is_err(),
        "must not archive a replacement inode"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        open_zip_source(&path, &inspected).is_err(),
        "must reject a replacement directory"
    );
    std::fs::remove_dir(&path).unwrap();
    use std::os::unix::ffi::OsStrExt;
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: native is a live, NUL-terminated path in this private fixture.
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    let (send, receive) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let _ = send.send(open_zip_source(&path, &inspected).is_err());
    });
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("replacement FIFO must not block source open")
    );
    task.join().unwrap();
    assert_eq!(std::fs::read(&retained).unwrap(), b"original");
}

#[test]
fn extraction_cancellation_discards_decoded_staging_before_merge() {
    for format in [
        ArchiveFormat::Zip,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        for existing in [false, true] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("source");
            std::fs::write(&source, b"archive contents").unwrap();
            let archive = root.path().join(format!("archive{}", format.extension()));
            create_archive(&[source], &archive, format).unwrap();
            let destination = root.path().join("destination");
            if existing {
                std::fs::create_dir(&destination).unwrap();
                std::fs::write(destination.join("sentinel"), b"untouched").unwrap();
            }
            let cancel = CancellationToken::new();
            cancel.cancel();
            let result = extract_archive_cancellable(&archive, &destination, &cancel);
            assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
            assert!(!destination.join("source").exists());
            if existing {
                assert_eq!(
                    std::fs::read(destination.join("sentinel")).unwrap(),
                    b"untouched"
                );
                assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
            } else {
                assert!(!destination.exists());
            }
            assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".gravityfile-extract-")
            }));
            assert!(matches!(
                extract_archive_cancellable(&archive, &destination, &cancel),
                Err(ArchiveError::Cancelled)
            ));
        }
    }
}

#[test]
fn extraction_reader_stops_after_midstream_cancellation() {
    struct CancelAfterRead {
        token: CancellationToken,
        calls: usize,
    }
    impl Read for CancelAfterRead {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            bytes[0] = b'x';
            self.token.cancel();
            Ok(1)
        }
    }
    let cancel = CancellationToken::new();
    let mut reader = ArchiveInput::new(
        CancelAfterRead {
            token: cancel.clone(),
            calls: 0,
        },
        &cancel,
    );
    let mut output = Vec::new();
    let error = reader.read_to_end(&mut output).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(output, b"x");
    assert_eq!(reader.reader.calls, 1);
    let mut seekable = ArchiveInput::new(std::io::Cursor::new(b"data"), &cancel);
    assert!(std::io::Seek::seek(&mut seekable, std::io::SeekFrom::Start(0)).is_err());
}

#[test]
fn archive_cancellation_respects_publication_boundary() {
    for format in [
        ArchiveFormat::Zip,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"contents").unwrap();
        let destination = root.path().join(format!("archive{}", format.extension()));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            create_archive_cancellable(
                std::slice::from_ref(&source),
                &destination,
                format,
                &cancel
            ),
            Err(ArchiveError::Cancelled)
        ));
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn archive_output_cancellation_stops_writes_without_interrupted_retry() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("payload");
    let cancel = CancellationToken::new();
    let mut output = create_archive_file(&path, &cancel).unwrap();
    output.write_all(b"before").unwrap();
    cancel.cancel();
    let error = std::io::copy(&mut std::io::Cursor::new(b"after"), &mut output).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(output.flush().is_err());
    assert!(std::io::Seek::seek(&mut output, std::io::SeekFrom::Start(0)).is_err());
    drop(output);
    assert_eq!(std::fs::read(path).unwrap(), b"before");
}

#[cfg(unix)]
#[test]
fn published_archives_keep_payload_private() {
    use std::os::unix::fs::PermissionsExt;
    for format in [
        ArchiveFormat::Zip,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarBz2,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
    ] {
        let root = TempDir::new().unwrap();
        let source = root.path().join("private");
        std::fs::write(&source, b"private contents").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let destination = root.path().join(format!("archive{}", format.extension()));
        create_archive(&[source], &destination, format).unwrap();
        let mode = std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "published {format:?} must not grant group/other access"
        );
        assert_eq!(mode & 0o7000, 0);
        let extracted = root.path().join("extracted");
        extract_archive(&destination, &extracted).unwrap();
        assert_eq!(
            std::fs::read(extracted.join("private")).unwrap(),
            b"private contents"
        );
    }
}

#[test]
fn test_create_archive_failure_leaves_no_partial_destination() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("missing.txt");
    let archive_path = temp_dir.path().join("failed.zip");

    let result = create_archive(&[missing], &archive_path, ArchiveFormat::Zip);

    assert!(result.is_err());
    assert!(!archive_path.exists());
    assert!(
        std::fs::read_dir(temp_dir.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".gravityfile-archive-")),
        "failed archive creation left a staging file"
    );
}

#[test]
fn test_create_archive_rejects_destination_inside_source() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("data.txt"), "data").unwrap();
    let archive_path = source.join("recursive.zip");

    let result = create_archive(
        std::slice::from_ref(&source),
        &archive_path,
        ArchiveFormat::Zip,
    );

    assert!(result.is_err());
    assert!(!archive_path.exists());
}

#[test]
fn test_archive_already_exists_error() {
    let temp_dir = TempDir::new().unwrap();

    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test content").unwrap();

    let archive_path = temp_dir.path().join("test.zip");

    create_archive(
        std::slice::from_ref(&test_file),
        &archive_path,
        ArchiveFormat::Zip,
    )
    .unwrap();

    let result = create_archive(&[test_file], &archive_path, ArchiveFormat::Zip);
    assert!(matches!(result, Err(ArchiveError::DestinationExists(_))));
}

#[test]
fn test_zip_path_validation_consistency() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test").unwrap();

    let archive_path = temp_dir.path().join("test.zip");
    create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

    let extract_dir = temp_dir.path().join("extracted");
    let result = extract_archive(&archive_path, &extract_dir);
    assert!(result.is_ok());
}

#[test]
fn default_limits_are_generous_but_bounded() {
    let limits = ExtractLimits::default();
    assert!((1_000.0..=100_000.0).contains(&limits.max_ratio));
    assert!((GIB..=1024 * GIB).contains(&limits.max_total_bytes));
    assert!(limits.max_entry_bytes <= limits.max_total_bytes);
    assert!((10_000..=100_000_000).contains(&limits.max_entries));
}

#[test]
fn custom_limits_reject_what_defaults_accept() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("big");
    std::fs::write(&source, vec![7u8; 4096]).unwrap();
    let archive = root.path().join("big.tar.zst");
    create_archive(&[source], &archive, ArchiveFormat::TarZst).unwrap();
    let tight = ExtractLimits {
        max_entry_bytes: 1024,
        ..ExtractLimits::default()
    };
    let destination = root.path().join("out");
    let error = extract_archive_with(&archive, &destination, &tight, &CancellationToken::new())
        .unwrap_err();
    assert!(
        matches!(error, ArchiveError::DecompressionBomb(_)),
        "{error}"
    );
    assert!(!destination.exists());
    extract_archive(&archive, &destination).unwrap();
    assert_eq!(std::fs::read(destination.join("big")).unwrap().len(), 4096);
}

#[cfg(unix)]
#[test]
fn tar_read_only_directories_receive_their_contents() {
    use std::os::unix::fs::PermissionsExt;
    let mut builder = tar::Builder::new(Vec::new());
    let mut directory = tar::Header::new_gnu();
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_mode(0o555);
    directory.set_size(0);
    directory.set_cksum();
    builder
        .append_data(&mut directory, "locked/", std::io::empty())
        .unwrap();
    let mut file = tar::Header::new_gnu();
    file.set_mode(0o644);
    file.set_size(4);
    file.set_cksum();
    builder
        .append_data(&mut file, "locked/file", b"data".as_slice())
        .unwrap();
    let root = TempDir::new().unwrap();
    let archive = root.path().join("locked.tar");
    std::fs::write(&archive, builder.into_inner().unwrap()).unwrap();
    let destination = root.path().join("out");
    extract_archive(&archive, &destination).unwrap();
    let locked = destination.join("locked");
    assert_eq!(std::fs::read(locked.join("file")).unwrap(), b"data");
    assert_eq!(
        std::fs::metadata(&locked).unwrap().permissions().mode() & 0o777,
        0o555
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn chained_symlinks_cannot_escape_the_destination() {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, target) in [("d/l1", ".."), ("d/esc", "l1/../..")] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name(target).unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, name, std::io::empty())
            .unwrap();
    }
    let root = TempDir::new().unwrap();
    let archive = root.path().join("escape.tar");
    std::fs::write(&archive, builder.into_inner().unwrap()).unwrap();
    let destination = root.path().join("out");
    let error = extract_archive(&archive, &destination).unwrap_err();
    assert!(error.to_string().contains("escapes"), "{error}");
    assert!(!destination.exists());
}

#[test]
fn extraction_merges_into_existing_directories_without_replacing() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("tree");
    std::fs::create_dir_all(source.join("sub")).unwrap();
    std::fs::write(source.join("sub/new.txt"), "new").unwrap();
    let archive = root.path().join("tree.zip");
    create_archive(std::slice::from_ref(&source), &archive, ArchiveFormat::Zip).unwrap();
    let destination = root.path().join("out");
    std::fs::create_dir_all(destination.join("tree/sub")).unwrap();
    std::fs::write(destination.join("tree/sub/existing.txt"), "keep").unwrap();
    let extracted = extract_archive(&archive, &destination).unwrap();
    assert!(extracted.contains(&destination.canonicalize().unwrap().join("tree/sub/new.txt")));
    assert_eq!(
        std::fs::read_to_string(destination.join("tree/sub/existing.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("tree/sub/new.txt")).unwrap(),
        "new"
    );
}

#[test]
fn zip_symlink_target_rejects_invalid_encoding_without_replacement() {
    for bytes in [b"".as_slice(), b"target\xff", b"target\0suffix"] {
        assert_eq!(
            zip_symlink_target(bytes).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        zip_symlink_target(" café ".as_bytes()).unwrap(),
        Path::new(" café ")
    );
}

#[cfg(unix)]
#[test]
fn zip_symlink_targets_preserve_significant_whitespace() {
    for target in [" target ", "target\n", "\ttarget", " "] {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join(target), b"exact target contents").unwrap();
        std::os::unix::fs::symlink(target, source.join("link")).unwrap();
        let archive = root.path().join("archive.zip");
        create_archive(&[source], &archive, ArchiveFormat::Zip).unwrap();
        let extracted = root.path().join("extracted");
        extract_archive(&archive, &extracted).unwrap();
        let link = extracted.join("source/link");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(target));
        assert_eq!(std::fs::read(link).unwrap(), b"exact target contents");
    }
}

#[cfg(unix)]
#[test]
fn zip_round_trip_preserves_regular_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o600, 0o640, 0o700, 0o755] {
        let root = TempDir::new().unwrap();
        let top = root.path().join("top");
        let directory = root.path().join("nested");
        std::fs::create_dir(&directory).unwrap();
        let nested = directory.join("script");
        for path in [&top, &nested] {
            std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let archive = root.path().join("archive.zip");
        create_archive(&[top, directory], &archive, ArchiveFormat::Zip).unwrap();
        let mut inventory = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
        for name in ["top", "nested/script"] {
            let entry = inventory.by_name(name).unwrap();
            assert_eq!(
                entry.unix_mode().unwrap() & 0o7777,
                mode,
                "stored mode for {name}"
            );
        }
        let extracted = root.path().join("extracted");
        extract_archive(&archive, &extracted).unwrap();
        for name in ["top", "nested/script"] {
            assert_eq!(
                std::fs::metadata(extracted.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                mode,
                "extracted mode for {name}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn test_permission_stripping() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test content").unwrap();

    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&test_file, perms).unwrap();

    let archive_path = temp_dir.path().join("test.zip");
    create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

    let extract_dir = temp_dir.path().join("extracted");
    extract_archive(&archive_path, &extract_dir).unwrap();

    let extracted_file = extract_dir.join("test.txt");
    assert!(extracted_file.exists());

    let extracted_perms = std::fs::metadata(&extracted_file).unwrap().permissions();
    let mode = extracted_perms.mode();
    assert_eq!(mode & 0o7000, 0);
}

#[cfg(unix)]
#[test]
fn test_symlink_in_tar_archive() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().unwrap();
    let source_dir = temp_dir.path().join("source");
    std::fs::create_dir(&source_dir).unwrap();

    let test_file = source_dir.join("target.txt");
    std::fs::write(&test_file, "target content").unwrap();

    let symlink_path = source_dir.join("link.txt");
    symlink("target.txt", &symlink_path).unwrap();

    let archive_path = temp_dir.path().join("test.tar");
    create_archive(
        std::slice::from_ref(&source_dir),
        &archive_path,
        ArchiveFormat::Tar,
    )
    .unwrap();

    assert!(archive_path.exists());

    let extract_dir = temp_dir.path().join("extracted");
    let result = extract_archive(&archive_path, &extract_dir);
    assert!(result.is_ok(), "extract failed: {:?}", result.err());
}

#[test]
fn test_symlink_validation_paths() {
    let safe_target = Path::new("subdir/file.txt");
    let has_parent = safe_target
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(!has_parent);

    let escape_target = Path::new("../../../etc/passwd");
    let has_parent2 = escape_target
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    assert!(has_parent2);

    let absolute_target = Path::new("/etc/passwd");
    assert!(absolute_target.is_absolute());
}
