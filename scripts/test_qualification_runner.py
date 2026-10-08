"""Qualification records reflect actual process outcomes and input changes."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).parent / 'qualification' / 'run-check.py'


class QualificationRunnerTests(unittest.TestCase):
    def invoke(self, root, command, inputs=(), patterns=()):
        argv = [sys.executable, str(RUNNER), '--output', str(root / 'report.json'),
                '--log', str(root / 'check.log')]
        for path in inputs:
            argv += ['--input', str(path)]
        for pattern in patterns:
            argv += ['--input-glob', str(pattern)]
        return subprocess.run(argv + ['--'] + command, capture_output=True, timeout=10)

    def test_patterns_detect_added_deleted_and_renamed_sources(self):
        for action in ['add', 'delete', 'rename']:
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / 'src'
                source.mkdir()
                original = source / 'original.rs'
                original.write_text('original')
                added = source / 'nested' / 'added.rs'
                command = [sys.executable, '-c',
                    'import pathlib,sys; old=pathlib.Path(sys.argv[1]); new=pathlib.Path(sys.argv[2]); '
                    'new.parent.mkdir(); action=sys.argv[3]; '
                    'new.write_text("added") if action=="add" else old.unlink() if action=="delete" else old.rename(new)',
                    str(original), str(added), action]
                result = self.invoke(root, command, patterns=[source / '**' / '*.rs'])
                self.assertEqual(result.returncode, 1, result.stderr)
                report = json.loads((root / 'report.json').read_text())
                self.assertEqual(report['exit_code'], 0)
                expected = ([added] if action == 'add' else [original] if action == 'delete'
                            else [original, added])
                self.assertEqual(report['changed_inputs'], sorted(str(p.absolute()) for p in expected))
                self.assertEqual(report['state'], 'failed')

    def test_patterns_deduplicate_and_ignore_unselected_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / 'source.rs'
            source.write_text('fixed')
            result = self.invoke(root, [sys.executable, '-c',
                'import pathlib,sys; pathlib.Path(sys.argv[1]).write_text("cache")', str(root / 'cache.bin')],
                inputs=[source], patterns=[root / '**' / '*.rs', root / '*.rs'])
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual(len(report['inputs_before']), 1)
            self.assertEqual(report['inputs_before'], report['inputs_after'])

    def test_unmatched_pattern_prevents_command_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.invoke(root, [sys.executable, '-c', 'print("must not run")'],
                                 patterns=[root / '*.missing'])
            self.assertEqual(result.returncode, 1)
            self.assertIn(b'matched no files', result.stderr)
            self.assertFalse((root / 'check.log').exists())

    def test_removed_selected_symlink_fails_even_when_target_survives(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'target.txt'
            target.write_text('source')
            link = root / 'source.rs'
            try:
                link.symlink_to(target)
            except OSError as error:
                self.skipTest(f'Symlink creation unavailable: {error}')
            result = self.invoke(root, [sys.executable, '-c',
                'import pathlib,sys; pathlib.Path(sys.argv[1]).unlink()', str(link)],
                patterns=[root / '*.rs'])
            self.assertEqual(result.returncode, 1, result.stderr)
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual(report['changed_inputs'], [str(link.absolute())])
            self.assertTrue(target.exists())

    def test_success_preserves_literal_argv_and_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'
            source.write_bytes(b'fixed input')
            command = [sys.executable, '-c', 'import sys; print(sys.argv[1]); print("diagnostic", file=sys.stderr)', '$(not-a-command); literal']
            result = self.invoke(root, command, [source])
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual(report['command'], command)
            self.assertEqual(report['state'], 'passed')
            self.assertEqual(report['exit_code'], 0)
            self.assertEqual(report['inputs_before'], report['inputs_after'])
            self.assertEqual(report['log']['sha256'], hashlib.sha256((root / 'check.log').read_bytes()).hexdigest())
            self.assertIn(b'$(not-a-command); literal', (root / 'check.log').read_bytes())

    def test_failure_is_not_overridden_by_success_words(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.invoke(root, [sys.executable, '-c', 'print("all checks passed"); raise SystemExit(7)'])
            self.assertEqual(result.returncode, 7)
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual((report['state'], report['exit_code']), ('failed', 7))

    def test_changed_or_deleted_inputs_fail_a_successful_command(self):
        for action in ['write_text("changed")', 'unlink()']:
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / 'source'
                source.write_text('before')
                result = self.invoke(root, [sys.executable, '-c',
                    f'import pathlib,sys; pathlib.Path(sys.argv[1]).{action}', str(source)], [source])
                self.assertEqual(result.returncode, 1)
                report = json.loads((root / 'report.json').read_text())
                self.assertEqual(report['exit_code'], 0)
                self.assertEqual(report['state'], 'failed')
                self.assertEqual(report['changed_inputs'], [str(source.resolve())])

    def test_launch_failure_is_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.invoke(root, [str(root / 'missing-program')])
            self.assertEqual(result.returncode, 1)
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual(report['state'], 'failed')
            self.assertIsNone(report['exit_code'])
            self.assertIn('launch_error', report)

    def test_report_created_during_command_is_never_replaced(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = [sys.executable, '-c',
                'import pathlib,sys; pathlib.Path(sys.argv[1]).write_text("concurrent evidence"); print("command completed")',
                str(root / 'report.json')]
            result = self.invoke(root, command)
            self.assertEqual(result.returncode, 1)
            self.assertEqual((root / 'report.json').read_text(), 'concurrent evidence')
            self.assertIn(b'command completed', (root / 'check.log').read_bytes())
            self.assertEqual({path.name for path in root.iterdir()}, {'report.json', 'check.log'})

    def test_existing_artifacts_and_input_overlap_prevent_execution(self):
        for name in ['report.json', 'check.log']:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / name
                path.write_text('retain')
                command = [sys.executable, '-c', 'raise SystemExit("must not run")']
                result = self.invoke(root, command)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(path.read_text(), 'retain')
                self.assertNotIn(b'must not run', result.stderr)
                result = self.invoke(root, command, [path])
                self.assertEqual(result.returncode, 1)
                self.assertIn(b'must not overlap', result.stderr)


if __name__ == '__main__':
    unittest.main()
