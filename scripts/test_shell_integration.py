"""Exercise installed shells against the real wrappers and a disposable host."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent


@unittest.skipUnless(os.name == 'posix', 'Unix shell wrappers')
class ShellIntegrationTests(unittest.TestCase):
    def run_wrapper(self, shell, name, status=0, fail_mktemp=False):
        if sys.platform == "darwin" and any(0xDC80 <= ord(c) <= 0xDCFF for c in name):
            self.skipTest("macOS filesystem rejects non-Unicode path components; covered on Linux")
        executable = shutil.which(shell)
        if executable is None:
            if os.environ.get('GRAVITYFILE_REQUIRE_SHELLS') == '1':
                self.fail(f'Required qualification shell {shell} is not installed')
            self.skipTest(f'{shell} is not installed')
        with tempfile.TemporaryDirectory(prefix='gravityfile-shell-test-') as temporary:
            root = Path(temporary).resolve()
            target = root / name
            target.mkdir()
            binaries = root / 'bin'
            binaries.mkdir()
            marker = root / 'invoked'
            host = binaries / 'gravityfile'
            host.write_text("""#!/usr/bin/env python3
import os, sys
from pathlib import Path
Path(os.environ['SHELL_TEST_MARKER']).touch()
path = next(arg.split('=', 1)[1] for arg in sys.argv[1:] if arg.startswith('--cwd-file='))
Path(path).write_bytes(os.fsencode(os.environ['SHELL_TEST_TARGET']))
sys.exit(int(os.environ['SHELL_TEST_STATUS']))
""")
            host.chmod(0o700)
            if fail_mktemp:
                mktemp = binaries / 'mktemp'
                mktemp.write_text('#!/bin/sh\nexit 9\n')
                mktemp.chmod(0o700)
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
                       SHELL_TEST_TARGET=str(target), SHELL_TEST_STATUS=str(status),
                       SHELL_TEST_MARKER=str(marker), SHELL_TEST_WRAPPER=str(SCRIPTS / ('grav.' + shell)))
            if shell == 'fish':
                command = 'source "$SHELL_TEST_WRAPPER"; grav; set result $status; printf "%s" "$PWD"; exit $result'
                arguments = [executable, '--no-config', '-c', command]
            else:
                command = 'source "$SHELL_TEST_WRAPPER"; grav; result=$?; printf "%s" "$PWD"; exit "$result"'
                arguments = [executable, '-f', '-c', command]
            result = subprocess.run(arguments, cwd=root, env=env, capture_output=True, timeout=10)
            expected_status = 9 if fail_mktemp else status
            self.assertEqual(result.returncode, expected_status, result.stderr)
            expected = root if expected_status else target
            self.assertEqual(result.stdout, os.fsencode(expected), result.stderr)
            self.assertEqual(marker.exists(), not fail_mktemp)


def install_cases():
    for shell in ('bash', 'zsh', 'fish'):
        for label, name, status, failed in (
            ('ordinary', 'space and dollar $', 0, False),
            ('newlines', 'line\nending\n\n', 0, False),
            ('native', os.fsdecode(b'native-\xff\n'), 0, False),
            ('failed_host', 'must-not-enter', 7, False),
            ('failed_mktemp', 'must-not-launch', 0, True),
        ):
            def case(self, shell=shell, name=name, status=status, failed=failed):
                self.run_wrapper(shell, name, status, failed)
            setattr(ShellIntegrationTests, f'test_{shell}_{label}', case)


install_cases()
if __name__ == '__main__':
    unittest.main()
