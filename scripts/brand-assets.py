#!/usr/bin/env python3
"""Import approved native brand assets, verify their hashes, or stage a package.

Import only: uv run --with fonttools==4.64.0 --with brotli==1.2.0 python ...
Verify and package use Python's standard library and never install desktop files.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'crates/tessera-shell/assets/brand'
BRAND_REV = '7bf512a0041b51b90b88652aae12bef6ead17b83'
GUI_REV = '3ba23948adbabb4288bb3bc6fa0d3e4e074861e2'
LOGOS = ('symbol-primary.svg', 'symbol-reversed.svg', 'app-icon-light.svg', 'app-icon-dark.svg')
WEIGHTS = {400: 'Regular', 500: 'Medium', 600: 'SemiBold', 700: 'Bold'}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def import_assets(brand, gui):
    import fontTools
    from fontTools.ttLib import TTFont
    from fontTools.varLib.instancer import instantiateVariableFont
    from fontTools.merge import Merger
    assert fontTools.__version__ == '4.64.0'
    inputs = []

    def source(repo, rev, relative):
        data = subprocess.run(['git', '-C', str(repo), 'show', rev + ':' + relative], check=True, capture_output=True).stdout
        assert (repo / relative).read_bytes() == data, relative + ' differs from pinned source'
        inputs.append({'commit': rev, 'path': relative, 'sha256': digest(data)})
        return data

    ASSETS.mkdir(parents=True, exist_ok=True)
    for name in LOGOS:
        (ASSETS / name).write_bytes(source(brand, BRAND_REV, 'design/brand/recommended/' + name))
    (ASSETS / 'brand-tokens.json').write_bytes(source(brand, BRAND_REV, 'design/brand/versions/tokens-1.1.0.json'))
    (ASSETS / 'interface-tokens.json').write_bytes(source(gui, GUI_REV, 'design/gui/interface-tokens.json'))
    fonts = ASSETS / 'fonts'
    fonts.mkdir(exist_ok=True)
    font_receipts = []
    for stem, family, notice in [('noto-sans', 'Noto Sans', 'notosans-OFL.txt'), ('cascadia-code', 'Cascadia Code', 'cascadiacode-OFL.txt')]:
        (fonts / notice).write_bytes(source(brand, BRAND_REV, 'design/brand/fonts/' + notice))
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            for subset in ('latin', 'cyrillic'):
                (tmp / (subset + '.woff2')).write_bytes(source(brand, BRAND_REV, 'design/brand/fonts/' + stem + '-' + subset + '.woff2'))
            for weight, style in WEIGHTS.items():
                paths = []
                expected_cmap = set()
                for subset in ('latin', 'cyrillic'):
                    font = TTFont(tmp / (subset + '.woff2'), recalcTimestamp=False)
                    expected_cmap.update(font.getBestCmap())
                    font = instantiateVariableFont(font, {'wght': weight}, inplace=True)
                    font.flavor = None
                    path = tmp / (subset + '.ttf')
                    font.save(path)
                    paths.append(str(path))
                merged = Merger().merge(paths)
                merged.recalcTimestamp = False
                # Fonts with distinct native styles, never two competing family subsets.
                # Family/subfamily pairs follow legacy four-style grouping; IDs16/17
                # retain the complete typographic family and exact weight.
                legacy_family = family if weight in (400, 700) else family + ' ' + style
                legacy_style = 'Bold' if weight == 700 else 'Regular'
                names = {1: legacy_family, 2: legacy_style, 4: family + ' ' + style,
                         6: family.replace(' ', '') + '-' + style, 16: family, 17: style}
                for name_id, value in names.items():
                    merged['name'].setName(value, name_id, 3, 1, 0x409)
                    merged['name'].setName(value, name_id, 1, 0, 0)
                merged['OS/2'].usWeightClass = weight
                merged['OS/2'].fsSelection &= ~(1 | 32 | 64)
                merged['OS/2'].fsSelection |= 32 if weight == 700 else 64
                merged['head'].macStyle = 1 if weight == 700 else 0
                assert set(merged.getBestCmap()) == expected_cmap
                assert set(map(ord, 'Tessera Привет Ёё')).issubset(expected_cmap)
                target = fonts / (stem + '-' + str(weight) + '.ttf')
                merged.save(target)
                restored = TTFont(target)
                assert set(restored.getBestCmap()) == expected_cmap
                font_receipts.append({'path': str(target.relative_to(ASSETS)), 'family': family,
                                      'weight': weight, 'format': 'TrueType', 'codepoints': len(expected_cmap)})
    files = [{'path': str(p.relative_to(ASSETS)), 'bytes': p.stat().st_size, 'sha256': digest(p.read_bytes())}
             for p in sorted(ASSETS.rglob('*')) if p.is_file() and p.name != 'manifest.json']
    manifest = {'schema': 'tessera-native-brand/v1', 'brand_delivery': 'B1.2.0', 'brand_tokens': '1.1.0',
                'interface_tokens': '2.0.0', 'brand_commit': BRAND_REV, 'gui_commit': GUI_REV,
                'transform': 'fontTools4.64.0: instantiate wght400/500/600/700, merge Latin+Cyrillic, emit native TTF; SVG bytes unchanged',
                'font_scope': 'Embedded upright Latin and Cyrillic. Other scripts use platform fallback; italic may be synthesized.',
                'inputs': inputs, 'fonts': font_receipts, 'files': files}
    (ASSETS / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    verify()


def verify():
    manifest = json.loads((ASSETS / 'manifest.json').read_text())
    assert manifest['schema'] == 'tessera-native-brand/v1'
    expected = {f['path'] for f in manifest['files']}
    actual = {str(p.relative_to(ASSETS)) for p in ASSETS.rglob('*') if p.is_file() and p.name != 'manifest.json'}
    assert expected == actual and len(expected) == len(manifest['files']), 'asset inventory differs'
    for entry in manifest['files']:
        p = ASSETS / entry['path']
        assert p.resolve().is_relative_to(ASSETS.resolve()) and not p.is_symlink()
        data = p.read_bytes()
        assert len(data) == entry['bytes'] and digest(data) == entry['sha256'], entry['path']
    print('Verified ' + str(len(expected)) + ' production assets')
    return manifest


def package(output):
    verify()
    output.mkdir(parents=True, exist_ok=False)
    shutil.copytree(ASSETS, output / 'share/tessera/brand')
    icons = output / 'share/icons/hicolor/scalable/apps'
    icons.mkdir(parents=True)
    shutil.copyfile(ASSETS / 'app-icon-light.svg', icons / 'tessera.svg')
    print('Staged assets and canonical app icon; no desktop installation: ' + str(output))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    imp = sub.add_parser('import')
    imp.add_argument('--brand-source', type=Path, required=True)
    imp.add_argument('--gui-source', type=Path, required=True)
    sub.add_parser('verify')
    pkg = sub.add_parser('package')
    pkg.add_argument('output', type=Path)
    args = parser.parse_args()
    if args.action == 'import':
        import_assets(args.brand_source, args.gui_source)
    elif args.action == 'verify':
        verify()
    else:
        package(args.output)
