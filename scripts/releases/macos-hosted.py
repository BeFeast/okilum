#!/usr/bin/env python3
"""Source-pinned shipping artifacts. Never imports credentials or publishes feeds."""
import argparse
import hashlib
import json
import os
import plistlib
from pathlib import Path
import re
import tempfile
import shutil
import subprocess


def source_identity(source, workflow):
    source = source or workflow
    if not all(re.fullmatch('[0-9a-f]{40}', s or '') for s in [source, workflow]):
        raise ValueError('Expected full product source and workflow SHA')
    subprocess.run(['git', 'merge-base', '--is-ancestor', source, workflow], check=True)
    return source


def route(env):
    lane = env.get('MACOS_LANE') or 'local'
    if lane not in ['local', 'hosted']:
        raise ValueError('Unsupported Mac delivery lane; no fallback')
    source = source_identity(env.get('PRODUCT_SOURCE'), env.get('GITHUB_SHA'))
    if source != env['GITHUB_SHA'] and (lane != 'hosted' or env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'):
        raise ValueError('Explicit older source requires manual hosted artifact route')
    if lane == 'hosted' and env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch':
        raise ValueError('Hosted scheduled shipping disabled until approved signing route exists; no local fallback')
    with open(env['GITHUB_OUTPUT'], 'a') as output:
        output.write(f'lane={lane}\nsource={source}\n')


def preflight(env):
    if env.get('GITHUB_REF') != 'refs/heads/main' or env.get('GITHUB_RUN_ATTEMPT') != '1':
        raise ValueError('Only first-attempt trusted main workflow can produce a shipping artifact')
    workflow = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if workflow != env.get('GITHUB_SHA'):
        raise ValueError('Workflow checkout identity mismatch')
    source = source_identity(env.get('PRODUCT_SOURCE'), workflow)
    for name in ['SHIPPING_BUILD', 'PRODUCER', 'PRODUCER_NUMBER', 'PRODUCER_ATTEMPT']:
        if not re.fullmatch('[1-9][0-9]*', env.get(name, '')):
            raise ValueError('Missing positive producer metadata: ' + name)
    if (int(env['SHIPPING_BUILD']) != 5000 + int(env['PRODUCER_NUMBER'])
            or env['PRODUCER_ATTEMPT'] != '1'):
        raise ValueError('Shipping version must use original Forgejo 5000+N')
    if subprocess.check_output(['uname', '-sm'], text=True).strip() != 'Darwin arm64':
        raise ValueError('Shipping requires native Darwin arm64')
    identity = env.get('OKILUM_SIGNING_IDENTITY', '')
    profile = env.get('OKILUM_NOTARY_PROFILE', '')
    if not identity or not profile:
        raise ValueError('Approved Developer ID identity and notary profile required before Cargo')
    keychain = env.get('OKILUM_SIGNING_KEYCHAIN')
    keys = [keychain] if keychain else []
    listing = subprocess.check_output(['security', 'find-identity', '-v', '-p', 'codesigning', *keys], text=True)
    if not any(identity in line and re.search(r'"Developer ID Application: .+ \([A-Z0-9]{10}\)"', line)
               for line in listing.splitlines()):
        raise ValueError('BLOCKED: hosted runner has no approved Developer ID private-key identity; no compilation')
    # Uses only an already-provisioned profile. Never store credentials or print bodies.
    subprocess.run(['xcrun', 'notarytool', 'history', '--keychain-profile', profile,
                    '--output-format', 'json', *(['--keychain', keychain] if keychain else [])], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=30)
    return source, workflow


def build(env):
    source, workflow = preflight(env)
    output = Path('dist/macos-shipping').resolve()
    if output.exists():
        raise ValueError('Shipping output already exists; never overwrite/reuse ambiguous work')
    output.mkdir(parents=True)
    # Preserve only reviewed delivery scripts before selecting the older product tree.
    tools = Path(tempfile.mkdtemp(prefix='okilum-delivery-tools-', dir=env.get('RUNNER_TEMP')))
    tool_hashes = {}
    for name in ['scripts/build-macos-ci.sh', 'scripts/ci/supervisor-bundle.sh',
                 'scripts/updater/sign-bundle.sh', 'scripts/updater/make-dmg.sh']:
        dest = tools / name; dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(name, dest)
        tool_hashes[name] = hashlib.sha256(dest.read_bytes()).hexdigest()
    subprocess.run(['git', 'checkout', '--detach', source], check=True)
    if subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() != source:
        raise ValueError('Product source checkout mismatch')
    build_env = {**env, 'OKILUM_BUILD_VERSION': env['SHIPPING_BUILD'], 'OKILUM_DELIVERY_TOOLS': str(tools)}
    subprocess.run(['bash', str(tools / 'scripts/build-macos-ci.sh'), str(output)], env=build_env, check=True)
    values = dict(line.split('=', 1) for line in (output / 'release.env').read_text().splitlines())
    tree = subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], text=True).strip()
    if (values['SOURCE_SHA'] != source or values['SOURCE_TREE'] != tree
            or values['BUILD'] != env['SHIPPING_BUILD']):
        raise ValueError('Executed shipping script returned wrong source/tree/build')
    plist = plistlib.loads((output / 'Okilum.app/Contents/Info.plist').read_bytes())
    if (plist.get('OkilumSourceCommit') != source or plist.get('OkilumSourceTree') != tree
            or plist.get('CFBundleVersion') != env['SHIPPING_BUILD']):
        raise ValueError('Actual app bundle identity differs from requested source/build')
    for name in ['notarization.json', 'notarization-dmg.json']:
        if json.loads((output / name).read_text())['status'] != 'Accepted':
            raise ValueError('Notarization not accepted')
    files = []
    for field in ['ARCHIVE', 'DMG']:
        path = Path(values[field]).resolve()
        if path.parent != output or not path.is_file() or path.is_symlink():
            raise ValueError('Shipping output outside exact artifact directory')
        files.append({'name': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                      'size': path.stat().st_size})
    receipt = {'platform': 'macos-arm64', 'source': source, 'tree': tree, 'workflow_sha': workflow,
               'build': int(env['SHIPPING_BUILD']), 'producer': env['PRODUCER'],
               'producer_number': env['PRODUCER_NUMBER'], 'producer_attempt': env['PRODUCER_ATTEMPT'], 'github_run': env['GITHUB_RUN_ID'],
               'signature': 'Developer ID + notarization Accepted + stapler/Gatekeeper verified by shipping script',
               'delivery_script_sha256': tool_hashes,
               'publication': 'NONE; QA artifact only, not Stable or beta feed', 'assets': files}
    (output / 'provenance.json').write_text(json.dumps(receipt, indent=2) + '\n')
    # Only release payloads and nonsecret provenance leave this job; no keychains/env.
    keep = {x['name'] for x in files} | {'provenance.json'}
    destination = output.parent / 'shipping-download'
    destination.mkdir()
    for name in keep:
        (output / name).rename(destination / name)
    print(json.dumps(receipt))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['route', 'preflight', 'build'])
    command = parser.parse_args().command
    {'route': route, 'preflight': preflight, 'build': build}[command](dict(os.environ))
