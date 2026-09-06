#!/usr/bin/env python3
"""Verify XpressClaw's compiled icon resources in a macOS app bundle."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import subprocess

ROOT = Path(__file__).resolve().parents[1]
COMPILED = ROOT / 'crates/xpressclaw-tauri/icons/macos/compiled'

# Every appearance the adaptive icon needs a stack for. Clear and Tinted are both
# derived from the tintable stack, so three keys cover all six system appearances.
REQUIRED_APPEARANCES = {'NSAppearanceNameAqua', 'NSAppearanceNameDarkAqua', 'ISAppearanceTintable'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def catalog_appearances(car):
    """Appearances that the compiled catalog actually carries an icon stack for."""
    dump = subprocess.run(['/usr/bin/assetutil', '--info', str(car)],
                          capture_output=True, text=True, check=True).stdout
    return {entry['Appearance'] for entry in json.loads(dump)
            if entry.get('AssetType') == 'IconImageStack' and 'Appearance' in entry}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--skip-signature', action='store_true')
    args = parser.parse_args()
    subprocess.run(['python3', str(ROOT / 'scripts/build-app-icon.py'), '--check'], check=True)
    contents = args.app.resolve() / 'Contents'
    info = plistlib.loads((contents / 'Info.plist').read_bytes())
    if info.get('CFBundleIconName') != 'XpressClaw':
        raise RuntimeError(f"Unexpected CFBundleIconName: {info.get('CFBundleIconName')!r}")
    if info.get('CFBundleIconFile') not in {'XpressClaw', 'XpressClaw.icns'}:
        raise RuntimeError(f"Unexpected CFBundleIconFile: {info.get('CFBundleIconFile')!r}")
    for name in ['Assets.car', 'XpressClaw.icns']:
        bundled = contents / 'Resources' / name
        if digest(bundled) != digest(COMPILED / name):
            raise RuntimeError(f'Bundled {name} does not match the committed compiled asset')
    appearances = catalog_appearances(contents / 'Resources' / 'Assets.car')
    missing = REQUIRED_APPEARANCES - appearances
    if missing:
        raise RuntimeError(f'Asset catalog is missing appearance variants: {sorted(missing)}. '
                           'The icon would render but could not follow the system appearance.')
    executable = contents / 'MacOS' / info['CFBundleExecutable']
    if not executable.is_file():
        raise RuntimeError(f'Missing bundle executable: {executable}')
    architectures = subprocess.run(['lipo', '-archs', str(executable)],
                                   capture_output=True, text=True, check=True).stdout.split()
    signature = 'skipped'
    if not args.skip_signature:
        subprocess.run(['codesign', '--verify', '--deep', '--strict', str(args.app.resolve())], check=True)
        signature = 'valid'
    print(f"RESOURCES  OK   source hashes, CFBundleIconName, CFBundleIconFile, "
          f"Assets.car, XpressClaw.icns, executable ({' '.join(architectures)}), signature {signature}")
    print(f"CATALOG    OK   appearance stacks present: {', '.join(sorted(appearances))}")
    print("APPEARANCE NOT CHECKED   Finder, Dock, and app-switcher appearance switching "
          "is a manual observation. This check does not make it.")

if __name__ == '__main__':
    main()
