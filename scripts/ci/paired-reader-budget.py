"""Supplemental diagnosis: no retry-to-green, fixed five alternating pairs.

Both executables retain their original 100/16 ms wall gates; main has only
unmarked scrolling, toolbar also has marked scrolling. No exception is inferred.
The same VM, synthetic fixture and original runner scripts serve every pair.
"""
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

root = Path.cwd()
output = root / 'diagnostics/paired-reader'
output.mkdir(parents=True, exist_ok=True)
cases = [
    ('main', '7a01531b792ca4cbd927ee2f4a66c33ef6205b0e'),
    ('toolbar', '4f4049ac78ef16d605f689e5d52fa7f0296c47fa'),
]
results = []
fixtures = []
(output / "host.json").write_text(json.dumps({k: os.environ.get(k) for k in ["COMPUTERNAME", "RUNNER_NAME", "ImageVersion", "GITHUB_RUN_ID"]}, indent=2))
for name, source in cases:
    exe = root / 'inputs' / name / 'okilum.exe'
    info = subprocess.check_output([str(exe), '--build-info'], text=True)
    assert source in info and 'gpui-test-support: no' in info, info
    (output / (name + '-build-info.txt')).write_text(info)
    script = subprocess.check_output(['git', 'show', source + ':scripts/ci/code-toolbar-native-app.py'])
    (output / (name + '-runner.py')).write_bytes(script)
    fixtures.append(ast.dump(next(n for n in ast.parse(script).body if isinstance(n, ast.FunctionDef) and n.name == 'fixture')))
assert fixtures[0] == fixtures[1], 'The two executables must receive the identical fixture' 
for iteration in range(5):
    for name, source in (cases if iteration % 2 == 0 else cases[::-1]):
        directory = output / f'{iteration}-{name}'
        directory.mkdir()
        env = dict(os.environ, RUNNER_TEMP=str(directory), OKILUM_SOURCE_COMMIT=source)
        with (directory / 'runner.log').open('w') as log:
            process = subprocess.run([sys.executable, str(output / (name + '-runner.py')), str(root / 'inputs' / name / 'okilum.exe')], env=env, stdout=log, stderr=subprocess.STDOUT, timeout=200)
        trace = (directory / 'code-toolbar-native-app.log').read_text(encoding='utf-8')
        samples = []
        for line in trace.splitlines():
            if not line.startswith('CODE2810_FRAME'):
                continue
            hit = re.search(r'marks=(true|false) sample=(\d+) wall_ms=([\d.]+).*draw_ms=([\d.]+) clear_ms=([\d.]+)', line)
            assert hit, line
            samples.append({'marks': hit[1] == 'true', 'sample': int(hit[2]), 'wall_ms': float(hit[3]), 'draw_ms': float(hit[4]), 'clear_ms': float(hit[5])})
        result = {'iteration': iteration, 'case': name, 'source': source, 'backend': 'legacy (both sources predate staging merge)', 'exit': process.returncode, 'sha256': hashlib.sha256((root / 'inputs' / name / 'okilum.exe').read_bytes()).hexdigest(), 'samples': samples}
        results.append(result)
        print(json.dumps(result), flush=True)
        (output / 'results.json').write_text(json.dumps(results, indent=2))
# A failure remains a failure in this diagnostic too. All fixed cases run so a
# single miss cannot hide the other half of the same-runner comparison.
assert len(results) == 10
assert all(r['exit'] == 0 for r in results), 'One or more original 100/16 ms budgets failed; inspect every retained trace'
