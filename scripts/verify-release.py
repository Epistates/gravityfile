#!/usr/bin/env python3
"""Validate the complete release artifact set before preparing upload assets."""
import argparse
from contextlib import ExitStack
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tarfile
import tempfile
import zipfile

TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
)
MAX_BINARY_BYTES = 512 * 1024 * 1024
REQUIRED_DOCUMENTS = ('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.html')
MAX_DOCUMENT_BYTES = 8 * 1024 * 1024
MAX_TAR_PADDING_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 2 * MAX_BINARY_BYTES + 16 * 1024 * 1024
MAX_TAR_HEADERS = 32
MAX_TAR_METADATA_BYTES = 64 * 1024
MAX_TAR_TOTAL_METADATA_BYTES = 256 * 1024
MAX_ZIP_METADATA_READ_BYTES = 1024 * 1024


class ZipMetadataReader:
    """Bound reads before ZipFile allocates its central-directory objects."""
    def __init__(self, stream):
        self.stream = stream
        self.remaining = MAX_ZIP_METADATA_READ_BYTES

    def read(self, size=-1):
        if self.remaining is None:
            return self.stream.read(size)
        if size is None or size < 0:
            size = self.remaining + 1
        elif size > self.remaining:
            raise ValueError('ZIP metadata read budget exceeded')
        data = self.stream.read(size)
        self.remaining -= len(data)
        if self.remaining < 0:
            raise ValueError('ZIP metadata read budget exceeded')
        return data

    def seek(self, offset, whence=0):
        return self.stream.seek(offset, whence)

    def tell(self):
        return self.stream.tell()

    def seekable(self):
        return self.stream.seekable()


def bounded_tar_info():
    """Give each archive independent limits before tarfile processes extensions."""
    class ReleaseTarInfo(tarfile.TarInfo):
        headers = 0
        metadata_bytes = 0

        # Sparse PAX metadata can dispatch to these parsers even when the next
        # physical header is a regular file. Reject before parsing sparse maps
        # or reading sparse 1.0 map data from the binary payload.
        def _proc_gnusparse_00(self, member, headers):
            raise ValueError('Sparse TAR encodings are not supported in release assets')

        def _proc_gnusparse_01(self, member, headers):
            raise ValueError('Sparse TAR encodings are not supported in release assets')

        def _proc_gnusparse_10(self, member, headers, archive):
            raise ValueError('Sparse TAR encodings are not supported in release assets')

        def _proc_member(self, archive):
            # tarfile processes PAX/GNU payloads here before yielding a member.
            # Guard this hook rather than frombuf, which newer Python bypasses.
            cls = type(self)
            member = self
            cls.headers += 1
            if cls.headers > MAX_TAR_HEADERS:
                raise ValueError('Too many physical TAR headers')
            if member.type in (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.SOLARIS_XHDTYPE,
                               tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK):
                if not 0 <= member.size <= MAX_TAR_METADATA_BYTES:
                    raise ValueError('Oversized TAR metadata header')
                cls.metadata_bytes += member.size
                if cls.metadata_bytes > MAX_TAR_TOTAL_METADATA_BYTES:
                    raise ValueError('TAR metadata budget exceeded')
            elif member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
                raise ValueError('Unsupported release TAR member type')
            elif not 0 <= member.size <= MAX_BINARY_BYTES:
                raise ValueError('Oversized TAR member header')
            return super()._proc_member(archive)

    return ReleaseTarInfo


def copy_archive(source, destination):
    """Bound staging disk use even if the download grows after inspection."""
    flags = (os.O_RDONLY | getattr(os, "O_BINARY", 0)
             | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    descriptor = os.open(source, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"non-regular archive: {source.name}")
        if metadata.st_size > MAX_ARCHIVE_BYTES:
            raise ValueError(f"oversized archive: {source.name}")
        stream = os.fdopen(descriptor, "rb")
        descriptor = None
        with stream, destination.open("xb") as output:
            remaining = MAX_ARCHIVE_BYTES
            while block := stream.read(min(1024 * 1024, remaining + 1)):
                if len(block) > remaining:
                    raise ValueError(f"oversized archive: {source.name}")
                output.write(block)
                remaining -= len(block)
    finally:
        if descriptor is not None:
            os.close(descriptor)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def expected_notice_header(target):
    source = Path(__file__).resolve().parent.parent
    paths = [source / 'Cargo.toml', *sorted((source / 'crates').glob('*/Cargo.toml'))]
    manifests = {p.relative_to(source).as_posix(): digest(p) for p in paths}
    manifests_hash = hashlib.sha256(json.dumps(manifests, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    provenance = {'schema_version': 2, 'target': target,
                  'lock_sha256': digest(source / 'Cargo.lock'),
                  'config_sha256': digest(source / 'about.toml'),
                  'manifests_sha256': manifests_hash}
    return ('<!doctype html>\n<!-- gravityfile-notices '
            + json.dumps(provenance, sort_keys=True, separators=(',', ':'))
            + ' -->\n').encode('utf-8')


def verify_contents(path, target):
    root = f"gravityfile-{target}"
    windows = target.endswith("windows-msvc")
    suffix = ".exe" if windows else ""
    binaries = {f"{root}/{name}{suffix}" for name in ("grav", "gravityfile")}
    expected = binaries | {f'{root}/{name}' for name in REQUIRED_DOCUMENTS}
    notice_name = f'{root}/THIRD_PARTY_NOTICES.html'
    notice_header = expected_notice_header(target)
    seen = set()
    with ExitStack() as stack:
        tar_end = 0
        if windows:
            source = stack.enter_context(path.open('rb'))
            reader = ZipMetadataReader(source)
            archive = stack.enter_context(zipfile.ZipFile(reader))
            if len(archive.infolist()) > len(expected) + 1:
                raise ValueError(f'{path.name}: too many ZIP members')
            reader.remaining = None  # Metadata parsed; binary reads are streamed below.
        else:
            decoded = stack.enter_context(gzip.open(path, "rb"))
            archive = stack.enter_context(tarfile.open(
                fileobj=decoded, mode="r:", tarinfo=bounded_tar_info()))
        entries = archive.infolist() if windows else archive
        for entry in entries:
            name = entry.filename if windows else entry.name
            directory = entry.is_dir() if windows else entry.isdir()
            if windows and entry.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                raise ValueError(f'{path.name}: unsupported ZIP compression')
            if directory and name.rstrip("/") == root:
                if (entry.file_size if windows else entry.size) != 0:
                    raise ValueError(f"{path.name}: archive root has unexpected data")
                if windows and (entry.external_attr >> 16 & 0o170000) not in (0, 0o040000):
                    raise ValueError(f"{path.name}: non-directory archive root")
                key = root
            else:
                key = name
                if key not in expected:
                    raise ValueError(f"{path.name}: unexpected member {name!r}")
                size = entry.file_size if windows else entry.size
                limit = MAX_BINARY_BYTES if key in binaries else MAX_DOCUMENT_BYTES
                if not 0 < size <= limit:
                    raise ValueError(f"{path.name}: invalid member size for {name}")
                if windows:
                    mode = entry.external_attr >> 16
                    if directory or (mode & 0o170000) not in (0, 0o100000):
                        raise ValueError(f"{path.name}: non-regular binary {name}")
                elif (not entry.isfile() or entry.mode & 0o7000
                      or (key in binaries and not entry.mode & 0o111)):
                    raise ValueError(f"{path.name}: invalid member type or permissions: {name}")
                stream = archive.open(entry) if windows else archive.extractfile(entry)
                with stream:
                    total = 0
                    body_digest = hashlib.sha256()
                    expected_body_digest = None
                    if key == notice_name:
                        prefix = stream.read(len(notice_header))
                        total += len(prefix)
                        if prefix != notice_header:
                            raise ValueError(f"{path.name}: notices do not match target, lockfile or configuration")
                        checksum_prefix, checksum_suffix = b'<!-- body-sha256 ', b' -->\n'
                        checksum_size = len(checksum_prefix) + 64 + len(checksum_suffix)
                        checksum = stream.read(checksum_size)
                        total += len(checksum)
                        if (not checksum.startswith(checksum_prefix)
                                or not checksum.endswith(checksum_suffix)
                                or len(checksum) != checksum_size):
                            raise ValueError(f"{path.name}: invalid notice body checksum")
                        expected_body_digest = checksum[len(checksum_prefix):-len(checksum_suffix)]
                    while block := stream.read(1024 * 1024):
                        total += len(block)
                        if key == notice_name:
                            body_digest.update(block)
                    if total != size:
                        raise ValueError(f"{path.name}: truncated binary {name}")
                    if (key == notice_name
                            and body_digest.hexdigest().encode("ascii") != expected_body_digest):
                        raise ValueError(f"{path.name}: notice body checksum mismatch")
            if key in seen:
                raise ValueError(f"{path.name}: duplicate member {name!r}")
            seen.add(key)
            if not windows:
                tar_end = entry.offset_data + ((entry.size + 511) // 512) * 512
        if not windows:
            # tarfile may stop on an invalid header as well as on a zero header.
            # Validate from the end of the last accepted member, not merely the
            # decoder's current (potentially read-ahead) position. Reading to EOF
            # also verifies gzip CRC/length trailers, including later members.
            decoded.seek(tar_end)
            padding = 0
            while block := decoded.read(8192):
                padding += len(block)
                if padding > MAX_TAR_PADDING_BYTES or any(block):
                    raise ValueError(f"{path.name}: invalid or excessive TAR end padding")
            if padding < 1024:
                raise ValueError(f"{path.name}: missing TAR end markers")
    if seen - {root} != expected:
        raise ValueError(f"{path.name}: missing required binaries or notices")


def verify_directory(path, expected, directories):
    """Validate one fixed directory level without descending into unknown input."""
    if not stat.S_ISDIR(path.lstat().st_mode):
        raise ValueError(f'artifact directory must be a real directory: {path}')
    seen = set()
    with os.scandir(path) as entries:
        for entry in entries:
            if entry.name not in expected or entry.name in seen:
                raise ValueError(f'unexpected artifact entry: {entry.name!r}')
            mode = entry.stat(follow_symlinks=False).st_mode
            valid = stat.S_ISDIR(mode) if directories else stat.S_ISREG(mode)
            if not valid:
                raise ValueError(f'invalid artifact entry type: {entry.name!r}')
            seen.add(entry.name)
    if seen != expected:
        raise ValueError(f'missing artifact entries: {sorted(expected - seen)}')


def prepare(source, destination):
    verify_directory(source, set(TARGETS), directories=True)
    archives = []
    for target in TARGETS:
        extension = ".zip" if target.endswith("windows-msvc") else ".tar.gz"
        archive = source / target / f"gravityfile-{target}{extension}"
        checksum = archive.with_name(archive.name + ".sha256")
        verify_directory(source / target, {archive.name, checksum.name}, directories=False)
        archives.append((target, archive, checksum))
    if destination.exists():
        raise ValueError(f"output already exists: {destination}")
    # Keep incomplete outputs private and invisible at the final upload path.
    with tempfile.TemporaryDirectory(prefix=".release-verify-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "assets"
        staging.mkdir()
        checksums = []
        for target, archive, checksum in archives:
            with checksum.open("rb") as stream:
                data = stream.read(257)
            if len(data) > 256:
                raise ValueError(f"oversized checksum file: {checksum.name}")
            record = data.decode("ascii").strip()
            match = re.fullmatch(r"([0-9a-f]{64})  " + re.escape(archive.name), record)
            if not match:
                raise ValueError(f"malformed checksum: {archive.name}")
            copied = staging / archive.name
            copy_archive(archive, copied)
            # Hash and inspect the private copy that will actually be published.
            # Reopening the download for validation would permit its replacement
            # between checksum verification, content inspection and copying.
            if digest(copied) != match[1]:
                raise ValueError(f"checksum mismatch: {archive.name}")
            verify_contents(copied, target)
            record = f"{match[1]}  {archive.name}\n"
            checksums.append(record)
            (staging / checksum.name).write_text(record, encoding="ascii")
        (staging / "SHA256SUMS").write_text("".join(checksums), encoding="ascii")
        staging.rename(destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        prepare(args.source, args.destination)
    except (OSError, EOFError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Release validation failed: {error}\n")
    print(f"Verified {len(TARGETS)} release packages in {args.destination}")
