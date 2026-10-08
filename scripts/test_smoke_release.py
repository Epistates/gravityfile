"""Portable path encoding and fail-closed checks for optimized-binary smoke tests."""
import importlib.util
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('smoke_release', Path(__file__).with_name('smoke-release.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class SmokeReleaseTests(unittest.TestCase):
    @unittest.skipUnless(os.name == 'posix', 'Unix surrogate-escape path encoding')
    def test_unix_native_bytes_are_not_lossily_encoded(self):
        value = smoke.native_path(os.fsdecode(b'file-\xff\n'))
        self.assertEqual(value, {'platform': 'unix', 'bytes': list(b'file-\xff\n')})

    def test_windows_native_units_include_surrogates_and_non_bmp_characters(self):
        with patch.object(smoke.os, 'name', 'nt'):
            value = smoke.native_path('A\ud800\U0001f600')
        self.assertEqual(value, {'platform': 'windows', 'wide': [65, 0xd800, 0xd83d, 0xde00]})

    def test_windows_fixture_paths_match_extended_drive_and_unc_names(self):
        self.assertEqual(smoke.windows_extended_path('C:\\fixture'), '\\\\?\\C:\\fixture')
        self.assertEqual(smoke.windows_extended_path('\\\\server\\share\\fixture'),
                         '\\\\?\\UNC\\server\\share\\fixture')
        self.assertEqual(smoke.windows_extended_path('\\\\?\\C:\\fixture'),
                         '\\\\?\\C:\\fixture')

    def test_smoke_requires_the_expected_exit_status(self):
        binary = Path('grav')
        for actual_success, expected_success in ((True, False), (False, True)):
            result = subprocess.CompletedProcess([], 0 if actual_success else 1, b'', b'injected')
            with patch.object(smoke.subprocess, 'run', return_value=result):
                with self.assertRaisesRegex(ValueError, 'unexpected exit'):
                    smoke.recovery_command(binary, '.', '--json', success=expected_success)
