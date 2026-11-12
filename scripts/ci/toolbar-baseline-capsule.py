"""Unpatched baseline inputs; atlas experiments are built only on QA VM150."""
import os
from pathlib import Path
import subprocess
import tarfile

root=Path.cwd()
output=Path(os.environ['RUNNER_TEMP'])
tracked=subprocess.check_output(['git','ls-files','-z'],text=True,encoding='utf-8').split('\0')
def keep(info):
    if '.git' in Path(info.name).parts or info.name.endswith('.pdb'):
        return None
    return info
with tarfile.open(output/'toolbar-baseline-workspace.tar.gz','w:gz',compresslevel=1) as archive:
    for name in tracked:
        if name and (root/name).is_file(): archive.add(root/name,arcname=name,filter=keep)
    for name in ['vendor/gpui-component','vendor/comrak-0.47.0','target/code-toolbar-release','target/windows-dist']:
        archive.add(root/name,arcname=name,filter=keep)
with tarfile.open(output/'toolbar-baseline-cargo.tar.gz','w:gz',compresslevel=1) as archive:
    registry=Path.home()/'.cargo/registry'
    archive.add(registry,arcname='registry',filter=keep)
print('BASELINE_INPUTS_ONLY_NO_GPU_PATCH',os.environ['OKILUM_SOURCE_COMMIT'],flush=True)
