"""Select a fresh Stable feed only through retained signed producer pins."""
import hashlib
import json
from pathlib import Path

PINS = {
    '0.1.17312': dict(build=17175, source='ef1930ca6dea87bd4d677c9a67a5579ac9477eb0',
        package_sha256='9cad40b3d89c0352599f3a5b53d7e6eab5f34a566f34c99f4ba03af0f1c4246d',
        setup_sha256='582d41495d891124e90e0399257b2db7ae88121f3b4d2dee4575bd52da8ade33',
        setup='/home/god/work/qa-evidence/morning-2026-10-26/mac17502-ci/final-okilum_windows_stable_Setup.exe'),
    '0.1.19179': dict(build=18194, source='e8ef13bca9553c5673d8a21819f4ff3be7680d51',
        package_sha256='bae280aa2c581e1e8da35c9700ef3ec3478dd8428882bf86c96c56e6c22ae621',
        setup_sha256='2f9b03f47e0a14e681ca49f7c227a4f107691ce898ec273e31fbbb9f3f021309',
        setup='/home/god/work/qa-evidence/windows-19179-signing/Setup.exe'),
}


def select(feed, pins=PINS, read_bytes=lambda path: Path(path).read_bytes()):
    full = max((a for a in feed['Assets'] if a['Type'] == 'Full'),
               key=lambda a: tuple(map(int, a['Version'].split('.'))))
    version = full['Version']
    pin = pins[version]  # Unknown Stable requires reviewed identity pins, never guessing.
    if full['SHA256'].lower() != pin['package_sha256']:
        raise ValueError('Live Stable package differs from signed producer pin')
    if hashlib.sha256(read_bytes(pin['setup'])).hexdigest() != pin['setup_sha256']:
        raise ValueError('Retained signed Setup digest mismatch')
    return dict(package=version, signed=True,
                **{k: v for k, v in pin.items() if k != 'setup'}), pin['setup']
