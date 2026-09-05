#!/usr/bin/env python3
"""Rebuild XpressClaw's macOS icon with Apple's own renderer and compiler."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import struct
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
ICON_ROOT = ROOT / 'crates/xpressclaw-tauri/icons/macos'
DOC = ICON_ROOT / 'XpressClaw.icon'
DEVELOPER = Path(os.environ.get('DEVELOPER_DIR', '/Applications/Xcode.app/Contents/Developer'))
RENDERER = DEVELOPER.parent / 'Applications/Icon Composer.app/Contents/Executables/ictool'
LEGACY_BASE_SIZES = (16, 32, 128, 256, 512)
LEGACY_PIXEL_SIZES = (16, 32, 64, 128, 256, 512, 1024)
PNG_SIGNATURE = b'\x89PNG\r\n\x1a\n'
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


def paeth(left, above, upper_left):
    estimate = left + above - upper_left
    distances = (abs(estimate - left), abs(estimate - above), abs(estimate - upper_left))
    return (left, above, upper_left)[distances.index(min(distances))]


def png_info(data):
    if not data.startswith(PNG_SIGNATURE):
        raise RuntimeError('Legacy ICNS contains a non-PNG image payload')
    offset = len(PNG_SIGNATURE)
    width = height = bit_depth = color_type = interlace = None
    compressed = bytearray()
    while offset + 12 <= len(data):
        length = struct.unpack('>I', data[offset:offset + 4])[0]
        kind = data[offset + 4:offset + 8]
        payload_start = offset + 8
        payload_end = payload_start + length
        if payload_end + 4 > len(data):
            raise RuntimeError('Legacy ICNS contains a truncated PNG chunk')
        payload = data[payload_start:payload_end]
        if kind == b'IHDR':
            if len(payload) != 13:
                raise RuntimeError('Legacy ICNS contains an invalid PNG header')
            width, height, bit_depth, color_type, _, _, interlace = struct.unpack('>IIBBBBB', payload)
        elif kind == b'IDAT':
            compressed.extend(payload)
        elif kind == b'IEND':
            break
        offset = payload_end + 4
    if (width, height, bit_depth, color_type, interlace) != (width, height, 8, 6, 0):
        raise RuntimeError('Legacy ICNS PNGs must be non-interlaced 8-bit RGBA')
    raw = zlib.decompress(compressed)
    stride = width * 4
    expected_length = height * (stride + 1)
    if len(raw) != expected_length:
        raise RuntimeError('Legacy ICNS PNG has an unexpected decoded size')
    previous = bytearray(stride)
    corner_alpha = []
    cursor = 0
    for row_index in range(height):
        filter_type = raw[cursor]
        encoded = raw[cursor + 1:cursor + 1 + stride]
        cursor += stride + 1
        row = bytearray(encoded)
        for index in range(stride):
            left = row[index - 4] if index >= 4 else 0
            above = previous[index]
            upper_left = previous[index - 4] if index >= 4 else 0
            if filter_type == 1:
                row[index] = (row[index] + left) & 0xff
            elif filter_type == 2:
                row[index] = (row[index] + above) & 0xff
            elif filter_type == 3:
                row[index] = (row[index] + ((left + above) // 2)) & 0xff
            elif filter_type == 4:
                row[index] = (row[index] + paeth(left, above, upper_left)) & 0xff
            elif filter_type != 0:
                raise RuntimeError(f'Legacy ICNS PNG uses unsupported filter {filter_type}')
        if row_index in (0, height - 1):
            corner_alpha.extend((row[3], row[-1]))
        previous = row
    return width, height, corner_alpha


def verify_legacy_icns(path):
    data = path.read_bytes()
    if len(data) < 8 or data[:4] != b'icns':
        raise RuntimeError(f'Invalid legacy ICNS header: {path}')
    declared_size = struct.unpack('>I', data[4:8])[0]
    if declared_size != len(data):
        raise RuntimeError(f'Legacy ICNS size header does not match file: {path}')
    offset = 8
    images = []
    chunk_types = set()
    while offset + 8 <= len(data):
        kind = data[offset:offset + 4]
        chunk_types.add(kind)
        chunk_size = struct.unpack('>I', data[offset + 4:offset + 8])[0]
        if chunk_size < 8 or offset + chunk_size > len(data):
            raise RuntimeError(f'Invalid legacy ICNS chunk {kind!r}')
        payload = data[offset + 8:offset + chunk_size]
        if payload.startswith(PNG_SIGNATURE):
            images.append(png_info(payload))
        offset += chunk_size
    sizes = {width for width, height, _ in images if width == height}
    missing = set(LEGACY_PIXEL_SIZES) - sizes
    # iconutil stores the 16x16 1x representation in the legacy ARGB ic04 chunk.
    if 16 in missing and b'ic04' in chunk_types:
        missing.remove(16)
    if missing:
        raise RuntimeError(f'Legacy ICNS is missing pixel representations: {sorted(missing)}')
    opaque = [size for width, height, alpha in images if width == height and (size := width) in LEGACY_PIXEL_SIZES and any(value != 0 for value in alpha)]
    if opaque:
        raise RuntimeError(f'Legacy ICNS has opaque corners at sizes: {sorted(set(opaque))}')

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
    verify_legacy_icns(ICON_ROOT / 'compiled/XpressClaw.icns')

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
    with tempfile.TemporaryDirectory(prefix='xpressclaw-icon-') as temporary:
        iconset = Path(temporary) / 'XpressClaw.iconset'
        iconset.mkdir()
        rendered = {}
        for size in LEGACY_PIXEL_SIZES:
            output = Path(temporary) / f'Default-{size}.png'
            run(RENDERER, DOC, '--export-image', '--output-file', output,
                '--platform', 'macOS', '--rendition', 'Default', '--width', str(size),
                '--height', str(size), '--scale', '1')
            rendered[size] = output
        for size in LEGACY_BASE_SIZES:
            shutil.copy2(rendered[size], iconset / f'icon_{size}x{size}.png')
            shutil.copy2(rendered[size * 2], iconset / f'icon_{size}x{size}@2x.png')
        run('/usr/bin/iconutil', '-c', 'icns', iconset, '-o', compiled / 'XpressClaw.icns')
    for name in ['Assets.car', 'XpressClaw.icns', 'partial-info.plist']:
        path = compiled / name
        if not path.is_file() or path.stat().st_size == 0:
            raise RuntimeError(f'Missing compiled output: {path}')
    tracked = sorted(ROOT / path for path in EXPECTED_MANIFEST_PATHS)
    manifest.write_text(json.dumps({str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in tracked}, indent=2) + '\n')
    verify_manifest(manifest)

if __name__ == '__main__':
    main()
