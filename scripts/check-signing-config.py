#!/usr/bin/env python3
"""Validate macOS signing configuration without printing secret values."""
import os
import sys

REQUIRED = (
    'APPLE_CERTIFICATE_BASE64', 'APPLE_CERTIFICATE_PASSWORD',
    'APPLE_DEVELOPER_ID', 'APPLE_ID', 'APPLE_ID_PASSWORD', 'APPLE_TEAM_ID',
)


def signing_mode(environment):
    present = [name for name in REQUIRED if environment.get(name)]
    if not present:
        return 'adhoc'
    missing = [name for name in REQUIRED if not environment.get(name)]
    if missing:
        raise ValueError('Incomplete macOS signing configuration; missing: ' + ', '.join(missing))
    return 'developer-id'


if __name__ == '__main__':
    try:
        print(signing_mode(os.environ))
    except ValueError as error:
        print(error, file=sys.stderr)
        sys.exit(1)
