#!/usr/bin/env python3
"""Run converter regressions with pinned dependencies in an isolated environment."""
from pathlib import Path
import os
import subprocess
import tempfile
import venv

directory = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix='okilum-manifest-tests-') as temporary:
    venv.create(temporary, with_pip=True)
    python = Path(temporary) / ('Scripts/python.exe' if os.name == 'nt' else 'bin/python')
    subprocess.run([str(python), '-m', 'pip', 'install', '--disable-pip-version-check',
                    '-r', str(directory / 'requirements.txt')], check=True, timeout=120)
    subprocess.run([str(python), '-m', 'unittest', 'discover', '-s', str(directory),
                    '-p', 'test_*.py', '-v'], check=True, timeout=60)
