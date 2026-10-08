"""Package namespace and build-cache lifecycle probes for the verification gate."""
import hashlib
import gzip
import importlib.util
import io
import os
import subprocess
import sys
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('packaged', Path(__file__).with_name('verify-packaged-workspace.py'))
packaged = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaged)


class PackageCacheTests(unittest.TestCase):
    def test_decoded_limit_includes_concatenated_gzip_members_and_checks_trailer(self):
        with mock.patch.object(packaged, 'MAX_PACKAGE_DECODED_BYTES', 4):
            with packaged.decoded_package(gzip.compress(b'1234')) as stream:
                self.assertEqual(stream.read(), b'1234')
            with self.assertRaisesRegex(ValueError, 'Decoded package exceeds'):
                with packaged.decoded_package(gzip.compress(b'1234') + gzip.compress(b'5')):
                    self.fail('oversized decoded stream accepted')
        broken = bytearray(gzip.compress(b'payload'))
        broken[-8] ^= 1
        for payload in [bytes(broken), gzip.compress(b'payload')[:-1]]:
            with self.assertRaises((OSError, EOFError)):
                with packaged.decoded_package(payload):
                    self.fail('corrupt gzip accepted')

    def test_package_input_has_an_actual_byte_limit(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'package.crate'
            with mock.patch.object(packaged, 'MAX_PACKAGE_ARCHIVE_BYTES', 4):
                path.write_bytes(b'1234')
                self.assertEqual(packaged.read_package_archive(path), b'1234')
                path.write_bytes(b'12345')
                with self.assertRaisesRegex(ValueError, 'Compressed package archive exceeds'):
                    packaged.read_package_archive(path)
                path.write_bytes(b'')
                self.assertEqual(packaged.read_package_archive(path), b'')

    @unittest.skipUnless(os.name == 'posix', 'Unix nonblocking FIFO and no-follow checks')
    def test_package_input_rejects_fifo_and_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fifo = root / 'fifo'
            os.mkfifo(fifo)
            # Run in a child so a nonblocking-open regression has a test deadline.
            code = ('import importlib.util,sys; '
                    's=importlib.util.spec_from_file_location("p",sys.argv[1]); '
                    'm=importlib.util.module_from_spec(s); s.loader.exec_module(m); '
                    'm.read_package_archive(sys.argv[2])')
            result = subprocess.run([sys.executable, '-c', code, packaged.__file__, str(fifo)],
                                    capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'Package archive must be a regular file', result.stderr)
            target = root / 'regular'
            target.write_bytes(b'contents')
            link = root / 'link'
            link.symlink_to(target)
            with self.assertRaises(OSError):
                packaged.read_package_archive(link)

    def test_space_failure_does_not_extract_retire_cache_or_compile(self):
        import json
        metadata = {'workspace_members': [], 'packages': [], 'target_directory': 'target'}
        with mock.patch.object(sys, 'argv', ['verify-packaged-workspace.py']), \
                mock.patch.object(packaged.subprocess, 'check_output', return_value=json.dumps(metadata)), \
                mock.patch.object(packaged.shutil, 'disk_usage', return_value=mock.Mock(free=0)), \
                mock.patch.object(packaged.tempfile, 'TemporaryDirectory') as temporary, \
                mock.patch.object(packaged, 'isolated_cache') as cache, \
                mock.patch.object(packaged.subprocess, 'run') as compile_command:
            with self.assertRaisesRegex(RuntimeError, 'Insufficient package-build disk space'):
                packaged.main()
            temporary.assert_not_called()
            cache.assert_not_called()
            compile_command.assert_not_called()

    def test_space_preflight_checks_both_volumes_and_exact_boundary(self):
        paths = [Path('build-volume'), Path('temp-volume')]
        for free_values in [(99, 100), (100, 99)]:
            with self.subTest(free_values=free_values), mock.patch.object(
                packaged.shutil, 'disk_usage',
                side_effect=[mock.Mock(free=value) for value in free_values],
            ):
                with self.assertRaisesRegex(RuntimeError, 'Insufficient package-build disk space'):
                    packaged.require_build_space(paths, 100)
        with mock.patch.object(packaged.shutil, 'disk_usage', return_value=mock.Mock(free=100)) as usage:
            packaged.require_build_space(paths, 100)
            self.assertEqual(usage.call_count, 2)
        with mock.patch.object(packaged.shutil, 'disk_usage', return_value=mock.Mock(free=0)):
            packaged.require_build_space(paths, 0)
        with self.assertRaises(ValueError):
            packaged.require_build_space(paths, -1)

    def test_invalid_target_preserves_previous_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            old = root / ('b' * 64)
            old.mkdir()
            (old / 'sentinel').write_text('keep')
            (root / ('a' * 64)).write_text('occupied')
            with self.assertRaises(FileExistsError):
                with packaged.isolated_cache(root, 'a' * 64):
                    self.fail('regular-file target accepted')
            self.assertEqual((old / 'sentinel').read_text(), 'keep')

    @unittest.skipIf(os.name == 'nt', 'Unix special-file and permission checks')
    def test_lock_rejects_links_and_fifo_without_changing_external_data(self):
        for kind in ('symlink', 'hardlink', 'fifo'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                external = root / 'external'
                external.write_bytes(b'')
                lock = root / '.verification.lock'
                if kind == 'symlink':
                    lock.symlink_to(external)
                elif kind == 'hardlink':
                    os.link(external, lock)
                else:
                    os.mkfifo(lock)
                with self.assertRaises(ValueError):
                    with packaged.isolated_cache(root, 'a' * 64):
                        self.fail('special lock accepted')
                self.assertEqual(external.read_bytes(), b'')
                self.assertFalse((root / ('a' * 64)).exists())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with packaged.isolated_cache(root, 'a' * 64):
                self.assertEqual((root / '.verification.lock').stat().st_mode & 0o777, 0o600)

    @unittest.skipUnless(hasattr(os, 'O_NOFOLLOW'), 'requires native no-follow open')
    def test_lock_rejects_leaf_symlink_substitution_during_open(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock = root / '.verification.lock'
            external = root / 'external'
            external.write_bytes(b'')
            original_open = os.open

            def substitute(path, flags, mode):
                lock.symlink_to(external)
                return original_open(path, flags, mode)

            with mock.patch.object(packaged.os, 'open', side_effect=substitute):
                with self.assertRaises(OSError):
                    with packaged.isolated_cache(root, 'a' * 64):
                        self.fail('substituted lock accepted')
            self.assertEqual(external.read_bytes(), b'')

    def test_cache_prunes_only_old_content_keys_and_preserves_current(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            active, obsolete = 'a' * 64, 'b' * 64
            for name in (active, obsolete, 'unrelated'):
                (root / name).mkdir()
                (root / name / 'sentinel').write_text(name)
            with packaged.isolated_cache(root, active) as target:
                self.assertEqual((target / 'sentinel').read_text(), active)
                self.assertFalse((root / obsolete).exists())
                self.assertTrue((root / 'unrelated' / 'sentinel').exists())
                with self.assertRaises(RuntimeError):
                    with packaged.isolated_cache(root, 'c' * 64):
                        self.fail('concurrent verifier acquired the cache')
                self.assertTrue((root / active / 'sentinel').exists())
            with packaged.isolated_cache(root, 'c' * 64) as target:
                self.assertTrue(target.is_dir())
                self.assertFalse((root / active).exists())

    def test_cache_releases_lock_on_failure_and_rejects_invalid_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(ValueError):
                with packaged.isolated_cache(root, '../outside'):
                    self.fail('invalid key accepted')
            with self.assertRaisesRegex(RuntimeError, 'injected'):
                with packaged.isolated_cache(root, 'a' * 64):
                    raise RuntimeError('injected')
            with packaged.isolated_cache(root, 'a' * 64):
                pass

    def test_cache_lock_is_released_after_process_exit_without_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            code = """
import importlib.util, os, pathlib, sys
spec = importlib.util.spec_from_file_location('packaged', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with module.isolated_cache(pathlib.Path(sys.argv[2]), 'a' * 64):
    os._exit(0)
"""
            subprocess.run([sys.executable, '-c', code,
                            str(Path(__file__).with_name('verify-packaged-workspace.py')), temporary],
                           check=True, timeout=10)
            with packaged.isolated_cache(Path(temporary), 'a' * 64):
                pass

    @unittest.skipIf(__import__('os').name == 'nt', 'symlink creation needs Windows privileges')
    def test_cache_does_not_follow_obsolete_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'cache'
            root.mkdir()
            outside = Path(temporary) / 'outside'
            outside.mkdir()
            (outside / 'sentinel').write_text('keep')
            (root / ('b' * 64)).symlink_to(outside, target_is_directory=True)
            with packaged.isolated_cache(root, 'a' * 64):
                self.assertTrue((root / ('b' * 64)).is_symlink())
                self.assertEqual((outside / 'sentinel').read_text(), 'keep')
            with self.assertRaises(ValueError):
                with packaged.isolated_cache(root, 'b' * 64):
                    self.fail('symlink target accepted')


class PackageExtractionTests(unittest.TestCase):
    def test_physical_metadata_rejects_before_tarfile_parser(self):
        def header(kind, payload=b''):
            info = tarfile.TarInfo('demo-1.0.0/metadata')
            info.type = kind
            info.size = len(payload)
            return info.tobuf(format=tarfile.GNU_FORMAT) + payload + bytes((-len(payload)) % 512)
        extension = header(tarfile.XHDTYPE, b'x' * (64 * 1024))
        regular = header(tarfile.REGTYPE)
        cases = [
            header(tarfile.XHDTYPE, b'x' * (64 * 1024 + 1)),
            header(tarfile.XHDTYPE) * 33,
            (extension + regular) * 65,
            regular * 20_001,
            header(tarfile.GNUTYPE_SPARSE),
            header(tarfile.XHDTYPE, b'25 GNU.sparse.map=invalid\n'),
            header(tarfile.XHDTYPE, b'14 size=999999\n'),
        ]
        for raw in cases:
            self.archive.write_bytes(gzip.compress(raw + bytes(1024)))
            with mock.patch.object(packaged.tarfile, 'open') as parser:
                with self.assertRaises(ValueError):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                parser.assert_not_called()
            self.assertEqual(list(self.destination.iterdir()), [])

    def test_tar_end_marker_rejects_missing_or_hidden_following_data(self):
        for raw in [b'', bytes(512), bytes(1024) + b'hidden']:
            with self.assertRaises(ValueError):
                packaged.preflight_package_tar(io.BytesIO(raw))
        stream = io.BytesIO(bytes(1024))
        self.assertIs(packaged.preflight_package_tar(stream), stream)
        self.assertEqual(stream.tell(), 0)

    def test_member_and_file_limits_reject_before_extraction(self):
        self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE),
                            ('demo-1.0.0/other', tarfile.REGTYPE)])
        for setting, limit, message in [('MAX_PACKAGE_MEMBERS', 1, 'member limit'),
                                         ('MAX_PACKAGE_FILE_BYTES', 1, 'file limit')]:
            with mock.patch.object(packaged, setting, limit):
                with self.assertRaisesRegex(ValueError, message):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                self.assertEqual(list(self.destination.iterdir()), [])
        with mock.patch.object(packaged, 'MAX_PACKAGE_MEMBERS', 2):
            packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')

    def test_sparse_and_directory_payloads_reject_before_extraction(self):
        for sparse in [True, False]:
            with tarfile.open(self.archive, 'w:gz', format=tarfile.PAX_FORMAT) as stream:
                manifest = tarfile.TarInfo('demo-1.0.0/Cargo.toml')
                manifest.size = 1
                stream.addfile(manifest, io.BytesIO(b'x'))
                info = tarfile.TarInfo('demo-1.0.0/payload')
                info.size = 1
                if sparse:
                    info.pax_headers = {'GNU.sparse.map': '0,1', 'GNU.sparse.realsize': '1000000'}
                else:
                    info.type = tarfile.DIRTYPE
                stream.addfile(info, io.BytesIO(b'x'))
            with self.assertRaisesRegex(ValueError, 'Sparse|directory carries'):
                packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
            self.assertEqual(list(self.destination.iterdir()), [])

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / 'package.crate'
        self.destination = self.root / 'extracted'
        self.destination.mkdir()

    def write_archive(self, entries):
        with tarfile.open(self.archive, 'w:gz') as stream:
            for name, kind in entries:
                info = tarfile.TarInfo(name)
                info.type = kind
                data = b'[package]\nname = "demo"\nversion = "1.0.0"\n'
                if kind == tarfile.REGTYPE:
                    info.size = len(data)
                    stream.addfile(info, io.BytesIO(data))
                else:
                    info.linkname = '/outside'
                    stream.addfile(info)

    def test_regular_package_extracts_and_digest_matches(self):
        self.write_archive([('demo-1.0.0/', tarfile.DIRTYPE),
                            ('demo-1.0.0/Cargo.toml', tarfile.REGTYPE)])
        digest = packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
        self.assertEqual(digest, hashlib.sha256(self.archive.read_bytes()).hexdigest())
        self.assertTrue((self.destination / 'demo-1.0.0/Cargo.toml').is_file())

    @unittest.skipIf(os.name == 'nt', 'symlink creation needs Windows privileges')
    def test_dangling_destination_is_rejected_before_extraction(self):
        self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE)])
        target = self.destination / 'demo-1.0.0'
        outside = self.root / 'missing'
        target.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'already exists'):
            packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
        self.assertTrue(target.is_symlink())
        self.assertFalse(outside.exists())

    def test_invalid_namespace_is_rejected_before_any_extraction(self):
        for name in ['other/file', '/demo-1.0.0/file', 'demo-1.0.0/../outside',
                     'demo-1.0.0/./file', 'demo-1.0.0//file', 'demo-1.0.0/file:stream',
                     'demo-1.0.0/a\\b', 'demo-1.0.0/Cargo.toml']:
            with self.subTest(name=name):
                self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE), (name, tarfile.REGTYPE)])
                with self.assertRaises(ValueError):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                self.assertEqual(list(self.destination.iterdir()), [])

    def test_windows_nonportable_names_are_rejected_before_extraction(self):
        names = ['CON', 'nul.txt', 'AUX.log', 'prn', 'COM1.rs', 'LPT9',
                 'COM¹.txt', 'LPT²', 'COM³', 'NUL .txt', 'name.', 'name ',
                 ' leading', 'a<b', 'a>b', 'a|b', 'a?b', 'a*b', 'a"b', 'control\x01']
        for name in names:
            with self.subTest(name=name):
                self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE),
                                    (f'demo-1.0.0/{name}', tarfile.REGTYPE)])
                with self.assertRaises(ValueError):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                self.assertEqual(list(self.destination.iterdir()), [])

    def test_case_and_unicode_aliases_include_implicit_parent_directories(self):
        for left, right in [('Readme', 'README'), ('Src/a.rs', 'src/b.rs'),
                            ('café.rs', 'cafe\u0301.rs'), ('Straße/a', 'STRASSE/b'),
                            ('src', 'SRC/child')]:
            with self.subTest(left=left, right=right):
                self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE),
                                    (f'demo-1.0.0/{left}', tarfile.REGTYPE),
                                    (f'demo-1.0.0/{right}', tarfile.REGTYPE)])
                with self.assertRaisesRegex(ValueError, 'Nonportable archive path collision'):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                self.assertEqual(list(self.destination.iterdir()), [])

    def test_portable_unicode_and_near_reserved_names_remain_supported(self):
        names = ['café-日本.rs', 'AUXiliary.txt', 'COM10.txt', '.github/config',
                 'src/a.rs', 'src/b.rs', 'file with spaces.txt']
        self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE),
                            *[(f'demo-1.0.0/{name}', tarfile.REGTYPE) for name in names]])
        packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
        for name in names:
            self.assertTrue((self.destination / 'demo-1.0.0' / name).is_file())

    def test_links_devices_and_file_parent_collisions_are_rejected(self):
        cases = [[('demo-1.0.0/special', kind)] for kind in
                 [tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE]]
        cases += [[('demo-1.0.0/src', tarfile.REGTYPE), ('demo-1.0.0/src/lib.rs', tarfile.REGTYPE)],
                  [('demo-1.0.0', tarfile.REGTYPE)]]
        for entries in cases:
            with self.subTest(entries=entries):
                self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE), *entries])
                with self.assertRaises(ValueError):
                    packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
                self.assertEqual(list(self.destination.iterdir()), [])

    def test_missing_manifest_and_existing_destination_are_rejected(self):
        self.write_archive([('demo-1.0.0/src/lib.rs', tarfile.REGTYPE)])
        with self.assertRaises(ValueError):
            packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
        self.write_archive([('demo-1.0.0/Cargo.toml', tarfile.REGTYPE)])
        existing = self.destination / 'demo-1.0.0'
        existing.mkdir()
        sentinel = existing / 'keep'
        sentinel.write_text('unchanged')
        with self.assertRaises(ValueError):
            packaged.extract_package(self.archive, self.destination, 'demo-1.0.0')
        self.assertEqual(sentinel.read_text(), 'unchanged')


if __name__ == '__main__':
    unittest.main()
