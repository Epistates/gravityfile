#!/usr/bin/env python3
"""Record one qualification command without inferring success from log text."""
import argparse
import datetime
import glob
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def matched_inputs(patterns, require_matches):
    paths = set()
    for pattern in patterns:
        # Keep the selected name, not just a symlink's target: removal of a
        # selected link must remain observable even if its target still exists.
        matches = {Path(name).absolute() for name in glob.iglob(pattern, recursive=True)
                   if Path(name).is_file()}
        if require_matches and not matches:
            raise ValueError(f'Input pattern matched no files: {pattern}')
        paths.update(matches)
    return paths


def run(command, output, log, inputs, input_globs=()):
    output, log = output.resolve(), log.resolve()
    paths = sorted({path.resolve() for path in inputs}
                   | matched_inputs(input_globs, require_matches=True))
    if output == log or output in paths or log in paths:
        raise ValueError('Report, log, and input paths must not overlap')
    if output.exists() or log.exists():
        raise ValueError('Refusing to overwrite an existing report or log')
    before = {str(path): digest(path) for path in paths}
    report = {
        'schema_version': 1, 'command': command, 'cwd': str(Path.cwd()),
        'started_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'inputs_before': before,
        'input_globs': list(input_globs),
    }
    started = time.monotonic()
    with log.open('xb') as stream:
        try:
            # argv is passed directly; shell expansion is never implicit.
            report['exit_code'] = subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT).returncode
        except OSError as error:
            report['exit_code'] = None
            report['launch_error'] = str(error)
        stream.flush()
        os.fsync(stream.fileno())
    report['elapsed_seconds'] = time.monotonic() - started
    after = {}
    errors = {}
    after_paths = set(paths) | matched_inputs(input_globs, require_matches=False)
    for path in sorted(after_paths):
        try:
            after[str(path)] = digest(path)
        except OSError as error:
            after[str(path)] = None
            errors[str(path)] = str(error)
    report['inputs_after'] = after
    report['input_read_errors'] = errors
    report['changed_inputs'] = [path for path in sorted(set(before) | set(after))
                                if path not in before or before[path] != after.get(path)]
    report['log'] = {'path': str(log), 'sha256': digest(log)}
    report['state'] = ('passed' if report['exit_code'] == 0
                       and not report['changed_inputs'] and not errors else 'failed')
    report['finished_at_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    staged = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=output.parent, delete=False) as stream:
            staged = Path(stream.name)
            json.dump(report, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        # Both paths are on the same filesystem. Link installation is atomic
        # and refuses an occupied name, including a concurrent writer's report.
        # If hard links are unsupported, fail without replacing any evidence.
        os.link(staged, output)
    finally:
        if staged is not None:
            staged.unlink(missing_ok=True)
    if report['state'] == 'passed':
        return 0
    code = report['exit_code']
    return (128 - code if code < 0 else code) if code else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--log', required=True, type=Path)
    parser.add_argument('--input', action='append', default=[], type=Path)
    parser.add_argument('--input-glob', action='append', default=[],
                        help='Quoted file pattern expanded before and after the command; supports **')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('a command is required after --')
    try:
        return run(command, args.output, args.log, args.input, args.input_glob)
    except (OSError, ValueError) as error:
        print(f'Qualification recording failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
