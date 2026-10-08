#!/usr/bin/env python3
"""Bundle the pinned GPUI runtime shaders for the unsigned native PR package."""
import json
from pathlib import Path
import shutil
import subprocess
import sys


def stage(output):
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1']))
    package = next(p for p in metadata['packages'] if p['name'] == 'gpui-pre-windows')
    if package['version'] != '0.3.3':
        raise RuntimeError('Recheck runtime HLSL packaging on GPUI upgrades')
    source = Path(package['manifest_path']).parent
    shaders = output / 'gpui-shaders' / 'src'
    shaders.mkdir(parents=True, exist_ok=True)
    for name in ('shaders.hlsl', 'color_text_raster.hlsl', 'alpha_correction.hlsl'):
        shutil.copyfile(source / 'src' / name, shaders / name)
    shutil.copyfile(source / 'LICENSE-APACHE', shaders.parent / 'LICENSE-APACHE')


if __name__ == '__main__':
    stage(Path(sys.argv[1]))
