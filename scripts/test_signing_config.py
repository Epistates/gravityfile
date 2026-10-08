"""Signing-mode validation must never disclose configured secret values."""
import importlib.util
from pathlib import Path
import unittest
import subprocess
import sys
import os

spec = importlib.util.spec_from_file_location('signing', Path(__file__).with_name('check-signing-config.py'))
signing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signing)


class SigningConfigTests(unittest.TestCase):
    def test_command_exit_status_and_output_are_safe_for_workflow_use(self):
        script = str(Path(__file__).with_name('check-signing-config.py'))
        for environment, code, output in [
            ({}, 0, 'adhoc\n'),
            ({name: 'not-a-real-secret' for name in signing.REQUIRED}, 0, 'developer-id\n'),
            ({signing.REQUIRED[0]: 'not-a-real-secret'}, 1, ''),
        ]:
            # Windows process startup may require these OS paths. Do not inherit
            # any signing configuration from the test runner's environment.
            child_env = {key: os.environ[key] for key in ('SystemRoot', 'WINDIR') if key in os.environ}
            result = subprocess.run([sys.executable, script], env=child_env | environment,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, code)
            self.assertEqual(result.stdout, output)
            self.assertNotIn('not-a-real-secret', result.stdout + result.stderr)

    def test_unconfigured_and_complete_modes(self):
        self.assertEqual(signing.signing_mode({}), 'adhoc')
        secrets = {name: 'value with "quotes" $(commands) and\nnewlines' for name in signing.REQUIRED}
        self.assertEqual(signing.signing_mode(secrets), 'developer-id')

    def test_every_partial_configuration_fails_without_secret_values(self):
        for mask in range(1, (1 << len(signing.REQUIRED)) - 1):
            secrets = {name: 'secret-value-do-not-print' for index, name in enumerate(signing.REQUIRED) if mask & (1 << index)}
            with self.subTest(mask=mask), self.assertRaises(ValueError) as caught:
                signing.signing_mode(secrets)
            self.assertNotIn('secret-value-do-not-print', str(caught.exception))
            for name in signing.REQUIRED:
                if name not in secrets:
                    self.assertIn(name, str(caught.exception))
