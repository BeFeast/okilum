#!/usr/bin/env python3
"""Post-job cancellation hook; never dispatches or changes Forgejo statuses."""
import importlib.util
import json
import os
from pathlib import Path
import re
import sys

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)


def main(receipt):
    data = json.loads(Path(receipt).read_text())
    sha, branch, workflow = data['sha'], data['branch'], data['workflow']
    # Only unique push refs created by these bridges, never main or release refs.
    if not re.fullmatch(r'[0-9a-f]{40}', sha) or not re.fullmatch(
            r'forgejo-(?:[a-z-]+-)?pr/[1-9][0-9]*/' + sha +
            r'-[1-9][0-9]*-[1-9][0-9]*-[0-9a-f]{32}', branch):
        raise ValueError('Invalid bridge receipt identity')
    if not re.fullmatch(r'\.github/workflows/forgejo-[a-z-]+\.yml', workflow):
        raise ValueError('Invalid bridge workflow')
    run_id = data['run_id']
    if run_id is not None and (type(run_id) is not int or run_id <= 0):
        raise ValueError('Invalid bridge run ID')
    # Reading a receipt must not overwrite it before cancellation.
    os.environ.pop('TESSERA_CANCELLATION_RECEIPT', None)
    owner = transport.Cancellation(transport.GitHub(os.environ['MIRROR_TOKEN']), branch, sha, workflow)
    owner.run_id = run_id
    owner.cancel()


if __name__ == '__main__':
    main(sys.argv[1])
