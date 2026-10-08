#!/usr/bin/env python3
"""Require an accepted notarytool JSON result before release packaging."""
import argparse
import json
from pathlib import Path
import sys
import uuid

MAX_RESULT_BYTES = 1024 * 1024


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate JSON field in notarization result')
        result[key] = value
    return result


def verify_result(path):
    with Path(path).open('rb') as source:
        data = source.read(MAX_RESULT_BYTES + 1)
    if len(data) > MAX_RESULT_BYTES:
        raise ValueError('Notarization result exceeds size limit')
    try:
        result = json.loads(data.decode('utf-8'), object_pairs_hook=unique_object)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError('Malformed notarization JSON result') from error
    if not isinstance(result, dict) or result.get('status') != 'Accepted':
        raise ValueError('Notarization result is not Accepted')
    submission_id = result.get('id')
    if not isinstance(submission_id, str):
        raise ValueError('Missing notarization submission UUID')
    try:
        normalized = str(uuid.UUID(submission_id))
    except ValueError as error:
        raise ValueError('Invalid notarization submission UUID') from error
    if normalized != submission_id.lower():
        raise ValueError('Invalid notarization submission UUID format')
    return normalized


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('result', type=Path)
    args = parser.parse_args()
    try:
        submission_id = verify_result(args.result)
    except OSError:
        print('Cannot read notarization result', file=sys.stderr)
        return 1
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    print(f'Notarization Accepted: {submission_id}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
