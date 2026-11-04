#!/usr/bin/env python3
"""Fail clearly before discovery if the Python test environment is incomplete."""
import importlib
from pathlib import Path
import sys


def check():
    requirements = Path(__file__).with_name('requirements.txt')
    for line in requirements.read_text().splitlines():
        distribution, version = line.split('==')
        module = {'PyYAML': 'yaml'}[distribution]
        remedy = f'{sys.executable} -m pip install -r {requirements}'
        try:
            loaded = importlib.import_module(module)
        except ImportError as error:
            raise RuntimeError(
                f'Missing required Python test dependency {distribution}=={version} '
                f'(import {module}). Run: {remedy}') from error
        actual = getattr(loaded, '__version__', None)
        if actual != version:
            raise RuntimeError(
                f'Required Python test dependency {distribution}=={version}; '
                f'found {actual!r}. Run: {remedy}')
        print(f'Python test dependency: {distribution}=={version}')


if __name__ == '__main__':
    try:
        check()
    except RuntimeError as error:
        sys.exit(str(error))
