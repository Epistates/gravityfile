"""Fail-closed release notarization verification with synthetic responses."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('verify-notarization.py')
spec = importlib.util.spec_from_file_location('notarization', SCRIPT)
notarization = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notarization)
SUBMISSION_ID = '2efe2717-52ef-43a5-96dc-0797e4ca1041'


class NotarizationTests(unittest.TestCase):
    def test_only_accepted_with_canonical_uuid_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            for status in ('Accepted', 'Invalid', 'In Progress', 'Rejected', '', None, 1):
                with self.subTest(status=status):
                    path.write_text(json.dumps({'status': status, 'id': SUBMISSION_ID}))
                    if status == 'Accepted':
                        self.assertEqual(notarization.verify_result(path), SUBMISSION_ID)
                    else:
                        with self.assertRaises(ValueError):
                            notarization.verify_result(path)
            for identifier in (None, 1, {}, '', 'private-response', SUBMISSION_ID.replace('-', '')):
                with self.subTest(identifier=identifier):
                    path.write_text(json.dumps({'status': 'Accepted', 'id': identifier}))
                    with self.assertRaises(ValueError):
                        notarization.verify_result(path)

    def test_malformed_duplicate_and_oversized_responses_fail(self):
        cases = [b'', b'private-response', b'\xff', b'[]', b'null', b'{}',
                 b'{"status":"Invalid","status":"Accepted"}',
                 b'[' * 2000, b' ' * (notarization.MAX_RESULT_BYTES + 1)]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            for data in cases:
                path.write_bytes(data)
                with self.subTest(length=len(data)), self.assertRaises(ValueError):
                    notarization.verify_result(path)

    def test_cli_exit_status_and_safe_output(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            for status, code in [('Accepted', 0), ('private-response', 1)]:
                path.write_text(json.dumps({'status': status, 'id': SUBMISSION_ID,
                                            'message': 'private-response'}))
                result = subprocess.run([sys.executable, str(SCRIPT), str(path)],
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, code)
                self.assertNotIn('private-response', result.stdout + result.stderr)
                self.assertEqual(result.stdout, f'Notarization Accepted: {SUBMISSION_ID}\n' if code == 0 else '')
            path.unlink()
            result = subprocess.run([sys.executable, str(SCRIPT), str(path)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stderr, 'Cannot read notarization result\n')
