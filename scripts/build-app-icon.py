#!/usr/bin/env python3
"""Rebuild XpressClaw's macOS icon with Apple's own renderer and compiler."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
ICON_ROOT = ROOT / 'crates/xpressclaw-tauri/icons/macos'
DOC = ICON_ROOT / 'XpressClaw.icon'
DEVELOPER = Path(os.environ.get('DEVELOPER_DIR', '/Applications/Xcode.app/Contents/Developer'))
EXPECTED_MANIFEST_PATHS = {
    'scripts/build-app-icon.py',
    'crates/xpressclaw-tauri/icons/macos/background.svg',
    'crates/xpressclaw-tauri/icons/macos/XpressClaw.icon/icon.json',
    'crates/xpressclaw-tauri/icons/macos/XpressClaw.icon/Assets/teal-network.svg',
    'crates/xpressclaw-tauri/icons/macos/XpressClaw.icon/Assets/white-network.svg',
    'crates/xpressclaw-tauri/icons/macos/compiled/Assets.car',
    'crates/xpressclaw-tauri/icons/macos/compiled/XpressClaw.icns',
    'crates/xpressclaw-tauri/icons/macos/compiled/partial-info.plist',
}

# Same 7-node networks and edges as the upstream mark, normalized on a 1024 canvas.
LEFT = [(322, 282), (418, 378), (226, 378), (322, 474), (226, 570), (418, 570), (322, 666)]
RIGHT = [(654, 282), (558, 378), (750, 378), (654, 474), (750, 570), (558, 570), (654, 666)]
LEFT_EDGES = [(0, 1), (1, 3), (2, 3), (3, 4), (3, 5), (5, 6)]
RIGHT_EDGES = [(0, 1), (2, 3), (3, 4), (5, 6)]

def svg(points, edges, color):
    # Center and enlarge the original proportions, retaining a generous system-mask margin.
    p = [(round((x - 488) * 1.16 + 512, 2), round((y - 474) * 1.16 + 512, 2)) for x, y in points]
    lines = ''.join(f'<path d="M {p[a][0]} {p[a][1]} L {p[b][0]} {p[b][1]}"/>' for a, b in edges)
    circles = ''.join(f'<circle cx="{x}" cy="{y}" r="43"/>' for x, y in p)
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><g stroke="{color}" stroke-width="22" stroke-linecap="round">{lines}</g><g fill="{color}">{circles}</g></svg>\n'

def run(*args):
    subprocess.run([str(a) for a in args], check=True, env={**os.environ, 'DEVELOPER_DIR': str(DEVELOPER)})

def verify_manifest(manifest):
    expected = json.loads(manifest.read_text())
    if set(expected) != EXPECTED_MANIFEST_PATHS:
        missing = sorted(EXPECTED_MANIFEST_PATHS - set(expected))
        unexpected = sorted(set(expected) - EXPECTED_MANIFEST_PATHS)
        raise RuntimeError(f'Unexpected icon manifest entries. Missing: {missing}; unexpected: {unexpected}')
    expected_source_paths = {
        ROOT / path for path in EXPECTED_MANIFEST_PATHS
        if '/compiled/' not in path and path != 'scripts/build-app-icon.py'
    }
    actual_source_paths = {
        path for path in ICON_ROOT.rglob('*')
        if path.is_file() and 'compiled' not in path.parts and path.name != '.DS_Store'
    }
    if actual_source_paths != expected_source_paths:
        unexpected = sorted(str(path.relative_to(ROOT)) for path in actual_source_paths - expected_source_paths)
        missing = sorted(str(path.relative_to(ROOT)) for path in expected_source_paths - actual_source_paths)
        raise RuntimeError(f'Unexpected icon source files. Missing: {missing}; unexpected: {unexpected}')
    for relative, digest in expected.items():
        path = ROOT / relative
        if not path.is_file():
            raise RuntimeError(f'Missing icon asset: {relative}')
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise RuntimeError(f'Icon output is stale or modified: {relative}. Run scripts/build-app-icon.py.')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sources-only', action='store_true')
    parser.add_argument('--check', action='store_true', help='Fail if compiled assets differ from the last verified source hashes')
    args = parser.parse_args()
    manifest = ICON_ROOT / 'compiled/source-hashes.json'
    if args.check:
        verify_manifest(manifest)
        print('Icon source and compiled asset hashes match.')
        return
    assets = DOC / 'Assets'
    assets.mkdir(parents=True, exist_ok=True)
    (assets / 'teal-network.svg').write_text(svg(LEFT, LEFT_EDGES, '#65BDCB'))
    (assets / 'white-network.svg').write_text(svg(RIGHT, RIGHT_EDGES, '#F5FAFF'))
    # Background is authored as a native fill in Composer, with this SVG as editable source.
    (ICON_ROOT / 'background.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path fill="#27364C" d="M0 0h1024v1024H0z"/></svg>\n')
    if not (DOC / 'icon.json').exists():
        raise RuntimeError('Restore the committed Icon Composer document before regenerating its assets.')
    if args.sources_only:
        return
    compiled = ICON_ROOT / 'compiled'
    compiled.mkdir(exist_ok=True)
    run(DEVELOPER / 'usr/bin/actool', DOC, '--compile', compiled, '--output-format', 'human-readable-text',
        '--notices', '--warnings', '--errors', '--output-partial-info-plist', compiled / 'partial-info.plist',
        '--app-icon', 'XpressClaw', '--include-all-app-icons', '--enable-on-demand-resources', 'NO',
        '--development-region', 'en', '--target-device', 'mac', '--minimum-deployment-target', '10.15', '--platform', 'macosx')
    for name in ['Assets.car', 'XpressClaw.icns', 'partial-info.plist']:
        path = compiled / name
        if not path.is_file() or path.stat().st_size == 0:
            raise RuntimeError(f'Missing compiled output: {path}')
    tracked = sorted(ROOT / path for path in EXPECTED_MANIFEST_PATHS)
    manifest.write_text(json.dumps({str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in tracked}, indent=2) + '\n')
    verify_manifest(manifest)

if __name__ == '__main__':
    main()
