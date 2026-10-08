#!/usr/bin/env python3
"""Build and test generated package contents together, without publishing them.

Run cargo package --workspace --allow-dirty --no-verify --locked --offline first.
This checks archive contents using local patches, not registry publication.
"""
from contextlib import contextmanager
import argparse
import gzip
import os
import re
import hashlib
import io
import json
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
import tomllib
import unicodedata


def require_build_space(paths, minimum_bytes):
    """Check headroom without claiming to reserve it or predict build size."""
    if minimum_bytes < 0:
        raise ValueError('Minimum free bytes must be non-negative')
    for path in paths:
        available = shutil.disk_usage(path).free
        if available < minimum_bytes:
            raise RuntimeError(
                f'Insufficient package-build disk space at {path}: '
                f'{available} bytes free, {minimum_bytes} required. '
                'Free space or explicitly adjust --min-free-bytes; '
                'this headroom check does not guarantee the build will fit.')

# Cross-platform package policy, independent of the verifier's host filesystem.
# https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
_RESERVED_NAMES = {'CON', 'PRN', 'AUX', 'NUL'} | {
    prefix + digit for prefix in ('COM', 'LPT') for digit in '123456789¹²³'
}


def portable_component(part):
    return (part not in ('', '.', '..') and not part.startswith(' ')
            and not part.endswith((' ', '.'))
            and not any(character in '<>:"\\|?*' or ord(character) < 32
                        or 0xD800 <= ord(character) <= 0xDFFF for character in part)
            and part.split('.')[0].rstrip(' ').upper() not in _RESERVED_NAMES)


MAX_PACKAGE_ARCHIVE_BYTES = 64 * 1024 * 1024
MAX_PACKAGE_DECODED_BYTES = 256 * 1024 * 1024
MAX_PACKAGE_FILE_BYTES = 64 * 1024 * 1024
MAX_PACKAGE_MEMBERS = 10_000


def preflight_package_tar(stream):
    """Bound physical metadata before TarFile follows extension chains."""
    stream.seek(0, os.SEEK_END)
    length = stream.tell()
    stream.seek(0)
    headers = extension_bytes = extension_chain = 0
    while True:
        block = stream.read(512)
        if len(block) != 512:
            raise ValueError('Package TAR is missing its end marker')
        if block == bytes(512):
            if stream.read(512) != bytes(512):
                raise ValueError('Package TAR requires two zero end blocks')
            while chunk := stream.read(1024 * 1024):
                if any(chunk):
                    raise ValueError('Unexpected data after package TAR end marker')
            stream.seek(0)
            return stream
        headers += 1
        if headers > 20_000:
            raise ValueError('Package TAR exceeds physical header limit')
        member = tarfile.TarInfo.frombuf(block, 'utf-8', 'surrogateescape')
        if member.size < 0:
            raise ValueError('Negative package TAR payload size')
        end = stream.tell() + ((member.size + 511) // 512) * 512
        if end > length:
            raise ValueError('Truncated package TAR payload')
        if member.type in (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME):
            extension_chain += 1
            extension_bytes += member.size
            if member.size > 64 * 1024 or extension_bytes > 4 * 1024 * 1024:
                raise ValueError('Package TAR extension metadata exceeds limit')
            if extension_chain > 32:
                raise ValueError('Package TAR extension chain exceeds limit')
            metadata = stream.read(member.size)
            # Conservatively reject this reserved namespace anywhere in PAX
            # records, before sparse-map parsers can consume it. A literal
            # occurrence in a PAX value is also rejected by this package policy.
            if member.type in (tarfile.XHDTYPE, tarfile.XGLTYPE) and b'GNU.sparse.' in metadata:
                raise ValueError('Sparse package metadata is unsupported')
            if member.type in (tarfile.XHDTYPE, tarfile.XGLTYPE) and b' size=' in metadata:
                raise ValueError('PAX size overrides are unsupported in packages')
        elif member.type in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
            extension_chain = 0
            if member.size > MAX_PACKAGE_FILE_BYTES:
                raise ValueError('Package member exceeds 64 MiB file limit')
            if member.type == tarfile.DIRTYPE and member.size:
                raise ValueError('Package directory carries unexpected payload')
        else:
            raise ValueError('Unsupported package TAR physical member type')
        stream.seek(end)


@contextmanager
def decoded_package(payload):
    """Validate the complete gzip stream before TAR parsing or extraction."""
    with tempfile.SpooledTemporaryFile(max_size=8 * 1024 * 1024) as decoded:
        total = 0
        with gzip.GzipFile(fileobj=io.BytesIO(payload)) as compressed:
            while True:
                chunk = compressed.read(min(1024 * 1024, MAX_PACKAGE_DECODED_BYTES - total + 1))
                if not chunk:
                    break
                total += len(chunk)
                if total > MAX_PACKAGE_DECODED_BYTES:
                    raise ValueError('Decoded package exceeds 256 MiB limit')
                decoded.write(chunk)
        decoded.seek(0)
        yield decoded


def read_package_archive(archive):
    """Take one bounded snapshot of a regular compressed package input."""
    flags = (os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
             | getattr(os, 'O_NONBLOCK', 0) | getattr(os, 'O_BINARY', 0))
    descriptor = os.open(archive, flags)
    with os.fdopen(descriptor, 'rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('Package archive must be a regular file')
        # Enforce actual bytes read, not just a mutable stat length.
        payload = stream.read(MAX_PACKAGE_ARCHIVE_BYTES + 1)
        if len(payload) > MAX_PACKAGE_ARCHIVE_BYTES:
            raise ValueError('Compressed package archive exceeds 64 MiB limit')
        return payload


def extract_package(archive, destination, directory):
    """Validate a locally generated package's namespace before extracting it."""
    payload = read_package_archive(archive)
    with decoded_package(payload) as decoded, tarfile.open(
            fileobj=preflight_package_tar(decoded), mode='r:') as stream:
        members = []
        for member in stream:
            if len(members) >= MAX_PACKAGE_MEMBERS:
                raise ValueError('Package exceeds 10000-member limit')
            if member.sparse is not None:
                raise ValueError('Sparse package members are unsupported')
            if member.size < 0 or member.size > MAX_PACKAGE_FILE_BYTES:
                raise ValueError('Package member exceeds 64 MiB file limit')
            if member.isdir() and member.size:
                raise ValueError('Package directory carries unexpected payload')
            members.append(member)
        names = set()
        files = set()
        portable_names = {}
        for member in members:
            name = member.name.rstrip('/') if member.isdir() else member.name
            parts = name.split('/')
            if len(name.encode('utf-8', errors='surrogatepass')) > 4096 or len(parts) > 128:
                raise ValueError('Package path exceeds length or depth limit')
            if parts[0] != directory or not all(portable_component(part) for part in parts):
                raise ValueError(f'Unexpected archive path: {member.name}')
            if not (member.isfile() or member.isdir()):
                raise ValueError(f'Unexpected archive member type: {member.name}')
            if name in names:
                raise ValueError(f'Duplicate archive path: {name}')
            # Track every prefix, including implicit parent directories. Two
            # differently spelled parents can alias even when leaves differ.
            normalized = []
            for index, part in enumerate(parts, 1):
                normalized.append(unicodedata.normalize('NFC', part).casefold())
                key = '/'.join(normalized)
                spelling = '/'.join(parts[:index])
                previous = portable_names.setdefault(key, spelling)
                if previous != spelling:
                    raise ValueError(f'Nonportable archive path collision: {previous} / {spelling}')
            names.add(name)
            if member.isfile():
                files.add(name)
        for name in names:
            parts = name.split('/')
            if any('/'.join(parts[:index]) in files for index in range(1, len(parts))):
                raise ValueError(f'Archive file is also a parent directory: {name}')
        if directory in files or f'{directory}/Cargo.toml' not in files:
            raise ValueError('Package must contain a regular Cargo.toml beneath its directory')
        # Reserve the root exclusively after preflight. Unlike exists(), mkdir
        # rejects dangling symlinks too and does not leave a check/create gap.
        try:
            (destination / directory).mkdir(mode=0o700)
        except FileExistsError as error:
            raise ValueError(f'Package destination already exists: {directory}') from error
        stream.extractall(destination, members=members, filter='data')
    return hashlib.sha256(payload).hexdigest()


@contextmanager
def cache_lock_file(path):
    """Open a regular, unshared lock without following a Unix leaf symlink."""
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        pass
    else:
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError('Package build cache lock must be a regular, unshared file')
    flags = os.O_RDWR | os.O_CREAT | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
    descriptor = os.open(path, flags, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError('Package build cache lock must be a regular, unshared file')
        lock = os.fdopen(descriptor, 'r+b')
        descriptor = None  # The stream now owns the descriptor.
        with lock:
            yield lock
    finally:
        if descriptor is not None:
            os.close(descriptor)


@contextmanager
def isolated_cache(cache_root, cache_key):
    """Hold one verifier lease while retaining only its content-addressed cache."""
    if not re.fullmatch(r'[0-9a-f]{64}', cache_key):
        raise ValueError('Invalid package build cache key')
    cache_root.mkdir(parents=True, exist_ok=True)
    if cache_root.is_symlink():
        raise ValueError('Package build cache root must not be a symlink')
    lock_path = cache_root / '.verification.lock'
    with cache_lock_file(lock_path) as lock:
        if lock.seek(0, os.SEEK_END) == 0:
            lock.write(b'0')
            lock.flush()
        lock.seek(0)
        try:
            if os.name == 'nt':
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise RuntimeError('Another packaged-source verifier is using this cache; retry after it finishes') from error
        try:
            target = cache_root / cache_key
            if target.is_symlink():
                raise ValueError('Package build cache target must not be a symlink')
            # Establish a usable destination before retiring previous results.
            target.mkdir(exist_ok=True)
            for previous in cache_root.iterdir():
                if (previous.name != cache_key and re.fullmatch(r'[0-9a-f]{64}', previous.name)
                        and not previous.is_symlink() and previous.is_dir()):
                    print('Retiring package build cache:', previous.name, flush=True)
                    shutil.rmtree(previous)
            yield target
        finally:
            lock.seek(0)
            if os.name == 'nt':
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--min-free-bytes', type=int, default=4 * 1024 ** 3,
                        help='Required free space on source-temp and build volumes '
                             '(default: 4 GiB; 0 disables the headroom check)')
    args = parser.parse_args()
    if args.min_free_bytes < 0:
        parser.error('--min-free-bytes must be non-negative')
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--offline'], cwd=root))
    members = set(metadata['workspace_members'])
    packages = [p for p in metadata['packages'] if p['id'] in members]
    # Package archives already exist in this target directory. Check before
    # extracting sources or retiring any previous verification cache.
    require_build_space([Path(metadata['target_directory']), Path(tempfile.gettempdir())],
                        args.min_free_bytes)
    records = []
    with tempfile.TemporaryDirectory(prefix='gravityfile-packaged-workspace-') as temporary:
        destination = Path(temporary)
        directories = []
        patches = []
        for package in sorted(packages, key=lambda p: p['name']):
            name, version = package['name'], package['version']
            directory = f'{name}-{version}'
            archive = Path(metadata['target_directory']) / 'package' / f'{directory}.crate'
            digest = extract_package(archive, destination, directory)
            with (destination / directory / 'Cargo.toml').open('rb') as stream:
                manifest = tomllib.load(stream)
            if (manifest['package']['name'], manifest['package']['version']) != (name, version):
                raise ValueError(f'Package identity mismatch: {archive}')
            directories.append(directory)
            patches.append(f'{json.dumps(name)} = {{ path = {json.dumps(directory)} }}')
            records.append({'package': name, 'version': version, 'archive_sha256': digest})
        (destination / 'Cargo.toml').write_text(
            '[workspace]\nresolver = "2"\nmembers = ' + json.dumps(directories)
            + '\n\n[patch.crates-io]\n' + '\n'.join(patches) + '\n')
        # Preserve the checkout's qualified dependency versions; package manifests
        # and every compiled source/test file come exclusively from the archives.
        shutil.copyfile(root / 'Cargo.lock', destination / 'Cargo.lock')
        # Cargo archives normalize source mtimes. Reusing a target directory
        # across different archive contents can therefore retain stale local
        # dependencies, even when extracting into a fresh workspace directory.
        cache_inputs = {
            'packages': records,
            'workspace_sha256': hashlib.sha256((destination / 'Cargo.toml').read_bytes()).hexdigest(),
            'lockfile_sha256': hashlib.sha256((destination / 'Cargo.lock').read_bytes()).hexdigest(),
        }
        cache_key = hashlib.sha256(json.dumps(cache_inputs, sort_keys=True).encode()).hexdigest()
        cache_root = Path(metadata['target_directory']) / 'packaged-verification'
        with isolated_cache(cache_root, cache_key) as target:
            common = ['--locked', '--offline', '--target-dir', str(target)]
            # Separate Cargo invocations prevent unrelated workspace members from
            # supplying features that the selected package forgot to declare.
            for record in records:
                for features in ([], ['--no-default-features']):
                    command = ['cargo', 'check', '--package', record['package'], '--lib', '--bins',
                               *features, *common]
                    print('Verifying package feature boundary:', ' '.join(command), flush=True)
                    subprocess.run(command, cwd=destination, check=True)
                record['checked_feature_sets'] = ['default', 'no-default-features']
            subprocess.run(['cargo', 'test', '--workspace', '--all-features', *common],
                           cwd=destination, check=True)
    print(json.dumps({'schema_version': 1, 'packages': records, 'build_cache_key': cache_key,
                      'verification': 'isolated packaged workspace tests with local internal patches',
                      'cache_retention': 'one content key under an exclusive verifier lock',
                      'minimum_free_bytes_preflight': args.min_free_bytes,
                      'registry_publication_verified': False}, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, EOFError, tarfile.TarError, ValueError, RuntimeError,
            subprocess.CalledProcessError) as error:
        raise SystemExit(f'Packaged workspace verification failed: {error}') from None
