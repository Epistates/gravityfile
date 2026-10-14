#!/usr/bin/env python3
"""Generate escaped dependency notices from a pinned cargo-about license graph."""
import argparse
import hashlib
import html
import json
from pathlib import Path
import subprocess
import tempfile

TOOL_VERSION = 'cargo-about 0.9.2'
# The published crate declares MIT but omits a license file. Keep this visible
# in the output; no copyright statement is invented from its author metadata.
CANONICAL_EXCEPTIONS = {
    ('opensesame', '0.1.1', 'MIT'),
    # These published bindings and upstream LICENSE.md declare MIT via a link
    # rather than bundling its complete text. Keep canonical attribution visible.
    ('objc2', '0.6.4', 'MIT'),
    ('objc2-encode', '4.1.0', 'MIT'),
    ('objc2-foundation', '0.3.2', 'MIT'),
}


def manifest_fingerprint(root):
    paths = [root / 'Cargo.toml', *sorted((root / 'crates').glob('*/Cargo.toml'))]
    records = {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
               for p in paths}
    return hashlib.sha256(json.dumps(records, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def render(data, target, lock_hash, config_hash, manifests_hash):
    packages = {p['package']['id']: p['package'] for p in data['crates']}
    if not packages or len(packages) != len(data['crates']):
        raise ValueError('empty or duplicate package inventory')
    covered = set()
    sections = []
    fallbacks = []
    escape = html.escape
    for license in data['licenses']:
        if not license['text'].strip() or not license['used_by']:
            raise ValueError('empty license text or attribution')
        users = []
        for used in license['used_by']:
            package = used['crate']
            if package['id'] not in packages:
                raise ValueError('license names an unknown package')
            covered.add(package['id'])
            label = f"{package['name']} {package['version']}"
            users.append(label)
            if not license.get('source_path'):
                key = (package['name'], package['version'], license['id'])
                if key not in CANONICAL_EXCEPTIONS:
                    raise ValueError(f'missing source license text: {label} ({license["id"]})')
                authors = ', '.join(package.get('authors', [])) or 'not declared'
                fallbacks.append(f'{label}: canonical {license["id"]} text; upstream license file unavailable. Declared package authors: {authors}.')
        sections.append((license['id'], sorted(users), license['text']))
    if covered != packages.keys():
        raise ValueError(f'packages missing license attribution: {sorted(packages.keys() - covered)}')
    # Fixed provenance header is checked by the publication gate. Escape HTML
    # delimiters even inside the comment; target strings must remain inert.
    provenance = json.dumps({'schema_version': 2, 'target': target,
                             'lock_sha256': lock_hash, 'config_sha256': config_hash,
                             'manifests_sha256': manifests_hash},
                            sort_keys=True, separators=(',', ':'))
    provenance = provenance.replace('&', '\\u0026').replace('<', '\\u003c').replace('>', '\\u003e')
    lines = ['<!doctype html>', f'<!-- gravityfile-notices {provenance} -->',
             '<html lang="en"><meta charset="utf-8">',
             '<title>Gravityfile dependency notices</title>',
             '<style>body{max-width:80em;margin:2em auto;padding:1em;font-family:system-ui}pre{white-space:pre-wrap;overflow-wrap:anywhere}</style>',
             '<h1>Gravityfile dependency notices</h1>',
             f'<p>Target: {escape(target)}. Cargo.lock SHA-256: {escape(lock_hash)}.</p>',
             '<p>Generated from the default-feature workspace dependency graph, including build dependencies. Project licenses are distributed alongside this file.</p>']
    if fallbacks:
        lines.append('<h2>Source-text limitations</h2><ul>')
        lines.extend(f'<li>{escape(item)}</li>' for item in sorted(fallbacks))
        lines.append('</ul>')
    for identifier, users, text in sorted(sections):
        lines.extend([f'<h2>{escape(identifier)}</h2>',
                      f'<p>{escape(", ".join(users))}</p>', f'<pre>{escape(text)}</pre>'])
    lines.append('</html>')
    body = '\n'.join(lines[2:]) + '\n'
    body_hash = hashlib.sha256(body.encode('utf-8')).hexdigest()
    return '\n'.join(lines[:2]) + f'\n<!-- body-sha256 {body_hash} -->\n' + body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cargo-about', default='cargo-about')
    args = parser.parse_args()
    version = subprocess.check_output([args.cargo_about, '--version'], text=True).strip()
    if version != TOOL_VERSION:
        raise ValueError(f'expected {TOOL_VERSION}, received {version}')
    root = Path(__file__).resolve().parent.parent
    lock = root / 'Cargo.lock'
    config = root / 'about.toml'
    before = lock.read_bytes()
    config_before = config.read_bytes()
    manifests_before = manifest_fingerprint(root)
    with tempfile.TemporaryDirectory(prefix='gravityfile-notices-') as temporary:
        raw = Path(temporary) / 'licenses.json'
        subprocess.run([args.cargo_about, 'generate', '--locked', '--fail', '--workspace',
                        '--target', args.target, '--format', 'json', '--output-file', str(raw)],
                       cwd=root, check=True)
        if (lock.read_bytes() != before or config.read_bytes() != config_before
                or manifest_fingerprint(root) != manifests_before):
            raise ValueError('notice inputs changed during generation')
        result = render(json.loads(raw.read_text(encoding='utf-8')), args.target,
                        hashlib.sha256(before).hexdigest(),
                        hashlib.sha256(config_before).hexdigest(), manifests_before)
    # A failed generation cannot leave a partial final notice file.
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=args.output.parent,
                                     delete=False) as staged:
        staging = Path(staged.name)
        try:
            staged.write(result)
            staged.close()
            staging.replace(args.output)
        finally:
            staging.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
