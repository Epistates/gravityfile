#!/usr/bin/env python3
"""Unix PTY smoke: silent/responding queries, navigation, quit and restoration.

Usage: python3 scripts/qualification/terminal-smoke.py target/release/grav
Uses disposable fixtures, simulated replies and a forced initialization failure.
No latency or graphics-rendering claim.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time


def probe(binary, responding, editor_status=None):
    with tempfile.TemporaryDirectory(prefix='gravityfile-terminal-') as temporary:
        root = Path(temporary) / 'fixture'
        (root / 'sub').mkdir(parents=True)
        (root / 'fixture-visible.txt').write_text('root')
        (root / 'sub' / 'deep-visible.txt').write_text('deep')
        editor = Path(temporary) / 'editor.py'
        editor_result = Path(temporary) / 'editor.json'
        editor.write_text("""#!/usr/bin/env python3
import json, os, select, sys, termios
from pathlib import Path
mode = termios.tcgetattr(0)
print('GRAVITYFILE_EDITOR_READY', flush=True)
if not select.select([0], [], [], 10)[0]:
    sys.exit(9)
text = sys.stdin.readline()
Path(os.environ['GRAVITYFILE_EDITOR_RESULT']).write_text(json.dumps({
    'input': text, 'canonical': bool(mode[3] & termios.ICANON),
    'echo': bool(mode[3] & termios.ECHO)}))
sys.exit(int(os.environ['GRAVITYFILE_EDITOR_STATUS']))
""")
        editor.chmod(0o700)
        master, slave = pty.openpty()
        process = None
        data = bytearray()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
            before = termios.tcgetattr(master)

            def setup():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            cwd_file = Path(temporary) / 'cwd'
            process = subprocess.Popen(
                [str(binary), str(root), '--scan', '--cwd-file', str(cwd_file)],
                stdin=slave, stdout=slave, stderr=slave,
                env=dict(os.environ, TERM='xterm-256color', EDITOR=str(editor), VISUAL=str(editor),
                         GRAVITYFILE_EDITOR_RESULT=str(editor_result),
                         GRAVITYFILE_EDITOR_STATUS=str(editor_status or 0)), preexec_fn=setup,
            )

            replied = False

            def read_until(predicate):
                nonlocal replied
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.1)[0]:
                        data.extend(os.read(master, 65536))
                    if responding and not replied and b'\x1b[5n' in data:
                        os.write(master, b'\x1b[6;20;10t\x1b[0n')
                        replied = True
                    if len(data) > 4 * 1024 * 1024:
                        raise RuntimeError('Terminal output exceeded 4 MiB')
                    if predicate():
                        return
                    if process.poll() is not None:
                        # Exit can race with the predicate's preceding poll.
                        # Drain queued restoration output before judging it.
                        while select.select([master], [], [], 0)[0]:
                            block = os.read(master, 65536)
                            if not block:
                                break
                            data.extend(block)
                        if predicate():
                            return
                        raise RuntimeError('TUI exited before expected terminal state')
                raise TimeoutError(f'TUI state timeout; responding={responding}; tail={bytes(data[-2000:])!r}')

            read_until(lambda: b'fixture-visible' in data)
            def command(text):
                prompt_start = len(data)
                os.write(master, b':')
                read_until(lambda: b'type command (q, cd, help...)' in data[prompt_start:])
                for key in text + b'\r':
                    os.write(master, bytes([key]))
                    time.sleep(0.05)

            start = len(data)
            command(b'cd sub')
            read_until(lambda: b'deep-visible' in data[start:])
            if editor_status is not None:
                editor_start = len(data)
                command(b'brn')
                read_until(lambda: b'GRAVITYFILE_EDITOR_READY' in data[editor_start:])
                os.write(master, b'editor owns this input\n')
                read_until(lambda: data.count(b'\x1b[?1049h') == 2
                           and b'deep-visible' in data[data.rfind(b'\x1b[?1049h'):])
            command(b'q')
            read_until(lambda: process.poll() is not None and b'\x1b[?1049l' in data)
            checks = {
                'graphics_query_observed': b'\x1b[5n' in data,
                'response_sent_if_requested': replied == responding,
                'exit_success': process.returncode == 0,
                'navigation': cwd_file.read_text().strip() == str((root / 'sub').resolve()),
                'termios_restored': before == termios.tcgetattr(master),
                'alternate_screen_entered': b'\x1b[?1049h' in data,
                'alternate_screen_left': b'\x1b[?1049l' in data,
                'fixture_intact': (root / 'fixture-visible.txt').read_text() == 'root'
                    and (root / 'sub' / 'deep-visible.txt').read_text() == 'deep',
            }
            if editor_status is not None:
                editor_data = json.loads(editor_result.read_text())
                checks['editor_received_input'] = editor_data['input'] == 'editor owns this input\n'
                checks['editor_received_restored_terminal'] = editor_data['canonical'] and editor_data['echo']
                checks['browser_reinitialized'] = data.count(b'\x1b[?1049h') == 2
            if not all(checks.values()):
                raise RuntimeError(f'Terminal checks failed: {checks}')
            return {'binary': binary.name, 'responding_terminal': responding,
                    'editor_exit_status': editor_status,
                              'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                              'checks': checks, 'output_bytes': len(data)}
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)


def probe_initialization_failure(binary):
    """Force EPIPE after raw mode starts, before alternate-screen setup succeeds."""
    with tempfile.TemporaryDirectory(prefix='gravityfile-terminal-failure-') as temporary:
        master, slave = pty.openpty()
        reader, writer = os.pipe()
        os.close(reader)
        process = None
        try:
            before = termios.tcgetattr(master)

            def setup():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            process = subprocess.Popen(
                [str(binary), temporary], stdin=slave, stdout=writer,
                stderr=subprocess.PIPE, preexec_fn=setup,
                env=dict(os.environ, TERM='xterm-256color'),
            )
            _, error = process.communicate(timeout=20)
            checks = {
                'exit_failure': process.returncode == 1,
                'diagnostic_reported': error.startswith(b'Error:'),
                'termios_restored': before == termios.tcgetattr(master),
                'fixture_intact': not any(Path(temporary).iterdir()),
            }
            if not all(checks.values()):
                raise RuntimeError(f'Initialization cleanup failed: {checks}; {error!r}')
            return {'binary': binary.name, 'scenario': 'initialization_output_failure',
                    'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                    'checks': checks}
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate()
            os.close(writer)
            os.close(master)
            os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    results = [probe(binary, responding) for responding in (False, True)]
    results.extend(probe(binary, False, status) for status in (0, 7))
    results.append(probe_initialization_failure(binary))
    print(json.dumps(results))


if __name__ == '__main__':
    main()
