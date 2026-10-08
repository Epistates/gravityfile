"""Regression tests for the release publication gate (no network or execution)."""
import hashlib
import gzip
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("verify_release", Path(__file__).with_name("verify-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
notice_spec = importlib.util.spec_from_file_location('notice_generator', Path(__file__).with_name('generate-notices.py'))
notice_generator = importlib.util.module_from_spec(notice_spec)
notice_spec.loader.exec_module(notice_generator)


class ReleaseGateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "downloads"
        self.destination = self.root / "final"
        self.archives = []
        for target in release.TARGETS:
            root = f"gravityfile-{target}"
            directory = self.source / target
            directory.mkdir(parents=True)
            windows = target.endswith("windows-msvc")
            path = directory / (root + (".zip" if windows else ".tar.gz"))
            if windows:
                with zipfile.ZipFile(path, "w") as archive:
                    for name in ("grav", "gravityfile"):
                        archive.writestr(f"{root}/{name}.exe", b"binary fixture")
                    for name in release.REQUIRED_DOCUMENTS:
                        archive.writestr(f"{root}/{name}", self.document(name, target))
            else:
                self.write_tar(path, target)
            self.checksum(path)
            self.archives.append(path)

    def document(self, name, target):
        if name == 'THIRD_PARTY_NOTICES.html':
            body = b'<html>notice fixture</html>'
            checksum = hashlib.sha256(body).hexdigest().encode('ascii')
            return release.expected_notice_header(target) + b'<!-- body-sha256 ' + checksum + b' -->\n' + body
        return b'license fixture'

    def write_tar(self, path, target, variant=None):
        root = f"gravityfile-{target}"
        with tarfile.open(path, "w:gz") as archive:
            directory = tarfile.TarInfo(root)
            directory.type = tarfile.DIRTYPE
            archive.addfile(directory)
            for name in ("grav", "gravityfile"):
                entry = tarfile.TarInfo(f"{root}/{name}")
                entry.mode = 0o755
                entry.size = 7
                if variant == "symlink" and name == "grav":
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = "/tmp/untrusted"
                    entry.size = 0
                if variant == "nonexecutable":
                    entry.mode = 0o644
                if variant == "setuid":
                    entry.mode = 0o4755
                archive.addfile(entry, io.BytesIO(b"fixture"))
                if variant == "duplicate":
                    archive.addfile(entry, io.BytesIO(b"fixture"))
            for name in release.REQUIRED_DOCUMENTS:
                if variant == f'missing:{name}':
                    continue
                entry = tarfile.TarInfo(f"{root}/{name}")
                entry.mode = 0o644
                content = self.document(name, target)
                entry.size = len(content)
                archive.addfile(entry, io.BytesIO(content))
            if variant == "traversal":
                entry = tarfile.TarInfo("../escape")
                archive.addfile(entry)

    def checksum(self, path):
        path.with_name(path.name + ".sha256").write_text(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n", encoding="ascii")

    def rejected(self):
        with self.assertRaises((ValueError, OSError, EOFError, zipfile.BadZipFile, tarfile.TarError)):
            release.prepare(self.source, self.destination)
        self.assertFalse(self.destination.exists())
        self.assertFalse(list(self.root.glob(".release-verify-*")))

    def test_complete_set_is_verified_and_combined_checksums_match(self):
        release.prepare(self.source, self.destination)
        self.assertEqual(len(list(self.destination.iterdir())), 11)
        for line in (self.destination / "SHA256SUMS").read_text().splitlines():
            digest, filename = line.split("  ")
            self.assertEqual(release.digest(self.destination / filename), digest)
        with self.assertRaises(ValueError):
            release.prepare(self.source, self.destination)

    def test_each_required_notice_is_enforced_on_tar_and_zip(self):
        for missing in release.REQUIRED_DOCUMENTS:
            with self.subTest(format='tar', missing=missing):
                self.write_tar(self.archives[0], release.TARGETS[0], f'missing:{missing}')
                with self.assertRaisesRegex(ValueError, 'missing required binaries or notices'):
                    release.verify_contents(self.archives[0], release.TARGETS[0])
            with self.subTest(format='zip', missing=missing):
                root = f'gravityfile-{release.TARGETS[-1]}'
                with zipfile.ZipFile(self.archives[-1], 'w') as archive:
                    for name in ('grav.exe', 'gravityfile.exe', *release.REQUIRED_DOCUMENTS):
                        if name != missing:
                            archive.writestr(f'{root}/{name}', self.document(name, release.TARGETS[-1]) if name in release.REQUIRED_DOCUMENTS else b'fixture')
                with self.assertRaisesRegex(ValueError, 'missing required binaries or notices'):
                    release.verify_contents(self.archives[-1], release.TARGETS[-1])

    def test_notice_sizes_have_a_separate_bound(self):
        with patch.object(release, 'MAX_DOCUMENT_BYTES', 1):
            for target, archive in zip(release.TARGETS, self.archives):
                with self.subTest(target=target), self.assertRaisesRegex(ValueError, 'invalid member size'):
                    release.verify_contents(archive, target)

    def test_notice_symlinks_are_rejected(self):
        target = release.TARGETS[0]
        root = f'gravityfile-{target}'
        with tarfile.open(self.archives[0], 'w:gz') as archive:
            entry = tarfile.TarInfo(f'{root}/LICENSE-MIT')
            entry.type = tarfile.SYMTYPE
            entry.linkname = '/untrusted'
            entry.size = 7
            archive.addfile(entry, io.BytesIO(b'fixture'))
        with self.assertRaisesRegex(ValueError, 'Unsupported release TAR member type'):
            release.verify_contents(self.archives[0], target)
        target = release.TARGETS[-1]
        with zipfile.ZipFile(self.archives[-1], 'w') as archive:
            entry = zipfile.ZipInfo(f'gravityfile-{target}/LICENSE-MIT')
            entry.create_system = 3
            entry.external_attr = 0o120777 << 16
            archive.writestr(entry, b'/untrusted')
        with self.assertRaisesRegex(ValueError, 'non-regular'):
            release.verify_contents(self.archives[-1], target)

    def test_generated_notices_are_bound_to_target_lock_and_config(self):
        source = Path(__file__).resolve().parent.parent
        lock_hash = hashlib.sha256((source / 'Cargo.lock').read_bytes()).hexdigest()
        config_hash = hashlib.sha256((source / 'about.toml').read_bytes()).hexdigest()
        package = {'id': 'example@1.0', 'name': 'example', 'version': '1.0'}
        data = {'crates': [{'package': package, 'license': 'MIT'}],
                'licenses': [{'id': 'MIT', 'text': 'license fixture',
                              'source_path': 'LICENSE', 'used_by': [{'crate': package}]}]}
        for target, path in [(release.TARGETS[0], self.archives[0]),
                             (release.TARGETS[-1], self.archives[-1])]:
            for mutation in ('none', 'target', 'lock', 'config', 'manifests', 'missing_header', 'truncated_body', 'corrupt_body', 'missing_body', 'missing_checksum', 'large_body'):
                data['licenses'][0]['text'] = 'x' * (2 * 1024 * 1024) if mutation == 'large_body' else 'license fixture'
                document = notice_generator.render(
                    data, 'wrong-target' if mutation == 'target' else target,
                    '0' * 64 if mutation == 'lock' else lock_hash,
                    '0' * 64 if mutation == 'config' else config_hash,
                    '0' * 64 if mutation == 'manifests' else notice_generator.manifest_fingerprint(source),
                ).encode()
                if mutation == 'missing_header':
                    document = b'<html>stale notices without provenance</html>'
                if mutation == 'truncated_body':
                    document = document[:-10]
                elif mutation == 'corrupt_body':
                    document = document.replace(b'license fixture', b'license changed')
                elif mutation == 'missing_body':
                    document = b'\n'.join(document.split(b'\n')[:3]) + b'\n'
                elif mutation == 'missing_checksum':
                    document = b'\n'.join(document.split(b'\n')[:2]) + b'\n'
                root = f'gravityfile-{target}'
                windows = target.endswith('windows-msvc')
                names = ['grav.exe', 'gravityfile.exe'] if windows else ['grav', 'gravityfile']
                contents = {name: b'binary fixture' for name in names}
                contents.update({name: self.document(name, target) for name in release.REQUIRED_DOCUMENTS})
                contents['THIRD_PARTY_NOTICES.html'] = document
                if windows:
                    with zipfile.ZipFile(path, 'w') as archive:
                        for name, content in contents.items():
                            archive.writestr(f'{root}/{name}', content)
                else:
                    with tarfile.open(path, 'w:gz') as archive:
                        for name, content in contents.items():
                            entry = tarfile.TarInfo(f'{root}/{name}')
                            entry.mode = 0o755 if name in names else 0o644
                            entry.size = len(content)
                            archive.addfile(entry, io.BytesIO(content))
                with self.subTest(target=target, mutation=mutation):
                    if mutation in ('none', 'large_body'):
                        release.verify_contents(path, target)
                    else:
                        error = 'notice body checksum' if mutation in ('truncated_body', 'corrupt_body', 'missing_body', 'missing_checksum') else 'notices do not match'
                        with self.assertRaisesRegex(ValueError, error):
                            release.verify_contents(path, target)

    def test_missing_target_rejected(self):
        self.archives[0].unlink()
        self.rejected()

    def test_download_replacement_after_validation_cannot_change_published_bytes(self):
        original = self.archives[0].read_bytes()
        inspect = release.verify_contents
        inspected = []

        def replace_download_after_inspection(path, target):
            inspect(path, target)
            inspected.append(path)
            if target == release.TARGETS[0]:
                self.write_tar(self.archives[0], target, "symlink")
                self.checksum(self.archives[0])

        with patch.object(release, 'verify_contents', side_effect=replace_download_after_inspection):
            release.prepare(self.source, self.destination)
        self.assertEqual((self.destination / self.archives[0].name).read_bytes(), original)
        self.assertTrue(all(path.parent.name == 'assets' for path in inspected))
        self.assertNotEqual(self.archives[0].read_bytes(), original)

    def test_corrupted_staged_copy_is_rejected_and_cleaned_up(self):
        copy = release.copy_archive

        def corrupt_copy(source, destination):
            copy(source, destination)
            with destination.open('ab') as output:
                output.write(b'changed during copy')

        with patch.object(release, 'copy_archive', side_effect=corrupt_copy):
            self.rejected()

    def test_download_swap_cannot_separate_digest_from_content_validation(self):
        archive = self.archives[0]
        valid = archive.read_bytes()
        self.write_tar(archive, release.TARGETS[0], 'symlink')
        unsafe = archive.read_bytes()
        self.checksum(archive)
        hash_archive = release.digest
        inspect = release.verify_contents

        def substitute_valid_download_after_hash(path):
            value = hash_archive(path)
            if path.name == archive.name:
                archive.write_bytes(valid)
            return value

        def restore_unsafe_download_after_inspection(path, target):
            inspect(path, target)
            if target == release.TARGETS[0]:
                archive.write_bytes(unsafe)

        with patch.object(release, 'digest', side_effect=substitute_valid_download_after_hash), \
                patch.object(release, 'verify_contents', side_effect=restore_unsafe_download_after_inspection):
            self.rejected()

    def test_partial_copy_failure_is_cleaned_up(self):
        def fail_copy(source, destination):
            destination.write_bytes(b'partial')
            raise OSError('injected copy failure')

        with patch.object(release, 'copy_archive', side_effect=fail_copy):
            self.rejected()

    def test_archive_copy_byte_limit_and_exact_boundary(self):
        source = self.root / 'copy-source'
        destination = self.root / 'copy-destination'
        source.write_bytes(b'12345678')
        with patch.object(release, 'MAX_ARCHIVE_BYTES', 8):
            release.copy_archive(source, destination)
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            destination.unlink()
            source.write_bytes(b'123456789')
            with self.assertRaisesRegex(ValueError, 'oversized archive'):
                release.copy_archive(source, destination)
            self.assertFalse(destination.exists())

    def test_archive_growth_after_stat_is_still_bounded(self):
        source = self.root / 'growing-source'
        destination = self.root / 'copy-destination'
        source.write_bytes(b'12345678')
        fstat = os.fstat

        def grow_after_stat(descriptor):
            metadata = fstat(descriptor)
            with source.open('ab') as output:
                output.write(b'9')
            return metadata

        with patch.object(release, 'MAX_ARCHIVE_BYTES', 8), \
                patch.object(release.os, 'fstat', side_effect=grow_after_stat):
            with self.assertRaisesRegex(ValueError, 'oversized archive'):
                release.copy_archive(source, destination)
        self.assertLessEqual(destination.stat().st_size, 8)

    def test_oversized_archive_leaves_no_release_or_staging(self):
        with patch.object(release, 'MAX_ARCHIVE_BYTES', 1):
            self.rejected()

    @unittest.skipUnless(os.name == 'posix', 'Unix nonblocking open required')
    def test_archive_copy_rejects_fifo_without_waiting_for_writer(self):
        source = self.root / 'fifo'
        os.mkfifo(source)
        with self.assertRaisesRegex(ValueError, 'non-regular archive'):
            release.copy_archive(source, self.root / 'copy-destination')

    def test_unexpected_artifact_rejected(self):
        (self.source / "surprise.zip").write_bytes(b"untrusted")
        self.rejected()

    def test_unexpected_empty_or_nested_directories_rejected_without_descent(self):
        for parent in (self.source, self.source / release.TARGETS[0]):
            with self.subTest(parent=parent):
                unexpected = parent / 'unexpected'
                unexpected.mkdir()
                self.rejected()
                (unexpected / 'nested').mkdir()
                scanned = []
                scandir = os.scandir

                def bounded_scandir(path):
                    scanned.append(Path(path))
                    self.assertNotEqual(Path(path), unexpected)
                    return scandir(path)

                # Call directly so TemporaryDirectory cleanup isn't part of
                # the traversal observation.
                with patch.object(release.os, 'scandir', side_effect=bounded_scandir):
                    with self.assertRaisesRegex(ValueError, 'unexpected artifact entry'):
                        release.prepare(self.source, self.destination)
                self.assertLessEqual(len(scanned), 2)
                shutil.rmtree(unexpected)

    def test_expected_regular_artifact_cannot_be_a_directory(self):
        archive = self.archives[0]
        archive.unlink()
        archive.mkdir()
        self.rejected()

    @unittest.skipUnless(os.name == 'posix', 'symlink creation requires Unix')
    def test_source_and_target_directory_symlinks_rejected(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.source, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'real directory'):
            release.prepare(alias, self.destination)
        target = self.source / release.TARGETS[0]
        moved = self.root / 'moved-target'
        target.rename(moved)
        target.symlink_to(moved, target_is_directory=True)
        self.rejected()

    def test_changed_archive_rejected(self):
        with self.archives[0].open("ab") as stream:
            stream.write(b"changed after build")
        self.rejected()

    def test_wrong_checksum_filename_rejected(self):
        archive = self.archives[0]
        archive.with_name(archive.name + ".sha256").write_text("0" * 64 + "  other.tar.gz\n")
        self.rejected()

    def test_unsafe_tar_members_rejected_even_with_matching_checksum(self):
        for variant in ("traversal", "symlink", "duplicate", "nonexecutable", "setuid"):
            with self.subTest(variant=variant):
                archive = self.archives[0]
                self.write_tar(archive, release.TARGETS[0], variant)
                self.checksum(archive)
                self.rejected()

    def test_zip_missing_binary_rejected_even_with_matching_checksum(self):
        archive = self.archives[-1]
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr(f"gravityfile-{release.TARGETS[-1]}/grav.exe", b"fixture")
        self.checksum(archive)
        self.rejected()

    def test_zip_metadata_budget_precedes_member_validation(self):
        archive = self.archives[-1]
        with zipfile.ZipFile(archive, 'w') as output:
            for index in range(300):
                output.writestr(f'entry-{index}', b'')
        with patch.object(release, 'MAX_ZIP_METADATA_READ_BYTES', 1024):
            with self.assertRaisesRegex(ValueError, 'ZIP metadata read budget exceeded'):
                release.verify_contents(archive, release.TARGETS[-1])

    def test_zip_metadata_budget_counts_rereads_and_bounds_read_to_eof(self):
        with patch.object(release, 'MAX_ZIP_METADATA_READ_BYTES', 8):
            reader = release.ZipMetadataReader(io.BytesIO(b'12345678'))
            self.assertEqual(reader.read(), b'12345678')
            reader.seek(0)
            with self.assertRaisesRegex(ValueError, 'ZIP metadata read budget exceeded'):
                reader.read(1)
            reader = release.ZipMetadataReader(io.BytesIO(b'123456789'))
            with self.assertRaisesRegex(ValueError, 'ZIP metadata read budget exceeded'):
                reader.read()

    def test_zip_unsupported_decoder_and_directory_payload_rejected(self):
        archive = self.archives[-1]
        target = release.TARGETS[-1]
        root = f'gravityfile-{target}'
        for compression, directory_data, message in [
            (zipfile.ZIP_BZIP2, b'', 'unsupported ZIP compression'),
            (zipfile.ZIP_STORED, b'hidden data', 'archive root has unexpected data'),
        ]:
            with self.subTest(compression=compression):
                with zipfile.ZipFile(archive, 'w', compression=compression) as output:
                    output.writestr(root + '/', directory_data)
                    for name in ('grav', 'gravityfile'):
                        output.writestr(f'{root}/{name}.exe', b'fixture')
                with self.assertRaisesRegex(ValueError, message):
                    release.verify_contents(archive, target)

    def test_deflated_zip_with_root_and_comment_passes(self):
        archive = self.archives[-1]
        target = release.TARGETS[-1]
        root = f'gravityfile-{target}'
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as output:
            output.comment = b'qualified fixture'
            output.writestr(root + '/', b'')
            for name in ('grav', 'gravityfile'):
                output.writestr(f'{root}/{name}.exe', b'fixture' * 1000)
            for name in release.REQUIRED_DOCUMENTS:
                output.writestr(f'{root}/{name}', self.document(name, target))
        self.checksum(archive)
        release.prepare(self.source, self.destination)

    def test_zip_corrupt_content_rejected_even_with_matching_archive_checksum(self):
        archive = self.archives[-1]
        contents = archive.read_bytes()
        self.assertIn(b"binary fixture", contents)
        archive.write_bytes(contents.replace(b"binary fixture", b"broken fixture", 1))
        self.checksum(archive)
        self.rejected()

    def test_oversized_checksum_rejected(self):
        archive = self.archives[0]
        archive.with_name(archive.name + ".sha256").write_text(" " * 257)
        self.rejected()

    def test_gzip_trailer_damage_rejected_with_matching_checksum(self):
        archive = self.archives[0]
        original = archive.read_bytes()
        for content in (original[:-1], original[:-8] + bytes(8)):
            with self.subTest(truncated=len(content) != len(original)):
                archive.write_bytes(content)
                self.checksum(archive)
                self.rejected()

    def test_huge_extension_is_rejected_before_reading_its_payload(self):
        for kind in (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME,
                     tarfile.GNUTYPE_LONGLINK):
            with self.subTest(kind=kind):
                header = tarfile.TarInfo('metadata')
                header.type = kind
                header.size = 2 ** 40
                # No payload: rejection must be the declared metadata size,
                # rather than a decoder failure after trying to read it.
                path = self.archives[0]
                path.write_bytes(gzip.compress(header.tobuf(format=tarfile.GNU_FORMAT)))
                with self.assertRaisesRegex(ValueError, 'Oversized TAR metadata'):
                    release.verify_contents(path, release.TARGETS[0])

    def test_chained_extension_headers_have_count_and_aggregate_limits(self):
        path = self.archives[0]
        original = gzip.decompress(path.read_bytes())
        header = tarfile.TarInfo('metadata')
        header.type = tarfile.XHDTYPE
        payload = b'13 comment=x\n'
        header.size = len(payload)
        extension = header.tobuf() + payload.ljust(512, b'\0')
        for limits, message in [
            ({'MAX_TAR_HEADERS': 4}, 'Too many physical TAR headers'),
            ({'MAX_TAR_TOTAL_METADATA_BYTES': 30}, 'TAR metadata budget exceeded'),
        ]:
            path.write_bytes(gzip.compress(extension * 5 + original))
            with self.subTest(limits=limits), patch.multiple(release, **limits):
                with self.assertRaisesRegex(ValueError, message):
                    release.verify_contents(path, release.TARGETS[0])

    def test_normal_pax_metadata_is_accepted_with_independent_archive_budgets(self):
        path = self.archives[0]
        root = f'gravityfile-{release.TARGETS[0]}'
        with tarfile.open(path, 'w:gz', format=tarfile.PAX_FORMAT) as archive:
            for name in ('grav', 'gravityfile', *release.REQUIRED_DOCUMENTS):
                member = tarfile.TarInfo(f'{root}/{name}')
                member.mode = 0o755
                content = self.document(name, release.TARGETS[0]) if name in release.REQUIRED_DOCUMENTS else b'fixture'
                member.size = len(content)
                member.pax_headers = {'mtime': '123456789.123456789'}
                archive.addfile(member, io.BytesIO(content))
        with patch.object(release, 'MAX_TAR_HEADERS', 10):
            for _ in range(2):
                release.verify_contents(path, release.TARGETS[0])

    def test_pax_sparse_formats_rejected_before_sparse_map_parsing(self):
        path = self.archives[0]
        target = release.TARGETS[0]
        root = f'gravityfile-{target}'
        variants = [
            {'GNU.sparse.size': '7', 'GNU.sparse.offset': 'invalid',
             'GNU.sparse.numbytes': 'invalid'},
            {'GNU.sparse.map': 'invalid,sparse,map'},
            {'GNU.sparse.major': '1', 'GNU.sparse.minor': '0'},
        ]
        for headers in variants:
            for global_header in (False, True):
                with self.subTest(headers=headers, global_header=global_header):
                    with tarfile.open(path, 'w:gz', format=tarfile.PAX_FORMAT,
                                      pax_headers=headers if global_header else None) as output:
                        for name in ('grav', 'gravityfile'):
                            member = tarfile.TarInfo(f'{root}/{name}')
                            member.mode = 0o755
                            member.size = 7
                            if not global_header:
                                member.pax_headers = headers
                            # Not a sparse 1.0 map: rejection must occur before
                            # the decoder attempts to interpret these bytes.
                            output.addfile(member, io.BytesIO(b'fixture'))
                    with self.assertRaisesRegex(ValueError, 'Sparse TAR encodings'):
                        release.verify_contents(path, target)
                    self.checksum(path)
                    self.rejected()

    def test_old_gnu_sparse_header_is_rejected_before_processing(self):
        header = tarfile.TarInfo('sparse')
        header.type = tarfile.GNUTYPE_SPARSE
        path = self.archives[0]
        path.write_bytes(gzip.compress(header.tobuf(format=tarfile.GNU_FORMAT)))
        with self.assertRaisesRegex(ValueError, 'Unsupported release TAR member type'):
            release.verify_contents(path, release.TARGETS[0])

    def test_later_gzip_member_trailer_is_verified(self):
        archive = self.archives[0]
        original = archive.read_bytes()
        archive.write_bytes(original + gzip.compress(bytes(512))[:-1])
        self.checksum(archive)
        self.rejected()

    def test_tar_end_markers_and_padding_are_validated(self):
        archive = self.archives[0]
        decoded = gzip.decompress(archive.read_bytes())
        # Root header plus two file headers with one data block each.
        end = 5 * 512
        for tail in (b"", bytes(512), b"not a header" + bytes(1024),
                     bytes(release.MAX_TAR_PADDING_BYTES + 1)):
            with self.subTest(tail_length=len(tail)):
                archive.write_bytes(gzip.compress(decoded[:end] + tail))
                self.checksum(archive)
                self.rejected()

    def test_valid_concatenated_gzip_members_are_accepted(self):
        archive = self.archives[0]
        decoded = gzip.decompress(archive.read_bytes())
        # Split inside a TAR header, rather than on a record boundary.
        archive.write_bytes(gzip.compress(decoded[:123]) + gzip.compress(decoded[123:]))
        self.checksum(archive)
        release.prepare(self.source, self.destination)

    @unittest.skipUnless(os.name == "posix" and shutil.which("tar"), "native Unix tar required")
    def test_native_tar_packaging_matches_the_release_contract(self):
        target = release.TARGETS[0]
        name = f"gravityfile-{target}"
        directory = self.root / name
        directory.mkdir()
        for binary in ("grav", "gravityfile"):
            path = directory / binary
            path.write_bytes(b"native tar fixture")
            path.chmod(0o755)
            if sys.platform == "darwin":
                subprocess.run(["xattr", "-w", "com.apple.ResourceFork",
                                "metadata excluded from release", str(path)], check=True)
        for document in release.REQUIRED_DOCUMENTS:
            (directory / document).write_bytes(self.document(document, target))
        archive = self.archives[0]
        subprocess.run(
            ["tar", "-czf", str(archive), "-C", str(self.root), name],
            env={**os.environ, "COPYFILE_DISABLE": "1"}, check=True,
        )
        self.checksum(archive)
        release.prepare(self.source, self.destination)


if __name__ == "__main__":
    unittest.main()
