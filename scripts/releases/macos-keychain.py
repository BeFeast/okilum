#!/usr/bin/env python3
"""Opt-in ephemeral hosted signer. No default/search-list or persistent host edits."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import ssl
import subprocess
import sys


def call(argv, **kwargs):
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=30, **kwargs).stdout


def root(env):
    for name in ['GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT']:
        if not re.fullmatch('[1-9][0-9]*', env.get(name, '')):
            raise ValueError('Missing run identity')
    temp = Path(env['RUNNER_TEMP']).resolve(strict=True)
    return temp / ('okilum-signing-' + env['GITHUB_RUN_ID'] + '-' + env['GITHUB_RUN_ATTEMPT'])


def validate(env):
    if (env.get('GITHUB_REF') != 'refs/heads/main' or env.get('GITHUB_RUN_ATTEMPT') != '1'
            or env.get('GITHUB_REPOSITORY') != 'BeFeast/okilum'):
        raise ValueError('Only first-attempt trusted main signer')
    source, workflow = env.get('PRODUCT_SOURCE', ''), env.get('GITHUB_SHA', '')
    if not all(re.fullmatch('[0-9a-f]{40}', s) for s in [source, workflow]):
        raise ValueError('Full source/workflow identities required')
    call(['git', 'merge-base', '--is-ancestor', source, workflow])
    if call(['git', 'rev-parse', 'HEAD']).decode().strip() != workflow:
        raise ValueError('Tooling checkout mismatch')
    if call(['uname', '-sm']).decode().strip() != 'Darwin arm64':
        raise ValueError('Native arm64 signer required')
    required = ['OKILUM_MAC_DEVELOPER_ID_P12', 'OKILUM_MAC_DEVELOPER_ID_PASSWORD',
                'OKILUM_MAC_NOTARY_APPLE_ID', 'OKILUM_MAC_NOTARY_PASSWORD',
                'OKILUM_MAC_NOTARY_TEAM', 'OKILUM_MAC_CERT_SHA256',
                'OKILUM_SIGNING_IDENTITY', 'OKILUM_NOTARY_PROFILE']
    if any(not env.get(k) for k in required):
        raise ValueError('Approved signing inputs unavailable; no compilation')
    if not re.fullmatch('[A-Z0-9]{10}', env['OKILUM_MAC_NOTARY_TEAM']):
        raise ValueError('Invalid signing team')
    if not re.fullmatch('[0-9A-Fa-f]{64}', env['OKILUM_MAC_CERT_SHA256']):
        raise ValueError('Frozen certificate SHA256 required')
    if not env['OKILUM_SIGNING_IDENTITY'].endswith('(' + env['OKILUM_MAC_NOTARY_TEAM'] + ')'):
        raise ValueError('Identity and approved team differ')
    return base64.b64decode(env['OKILUM_MAC_DEVELOPER_ID_P12'], validate=True)


def certificate_identity(pem, listing, env):
    certs = re.findall(r'-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----', pem, re.S)
    matches = []
    for cert in certs:
        der = ssl.PEM_cert_to_DER_cert(cert)
        if hashlib.sha256(der).hexdigest().lower() == env['OKILUM_MAC_CERT_SHA256'].lower():
            sha1 = hashlib.sha1(der).hexdigest().upper()
            matches += [line for line in listing.splitlines()
                        if sha1 in line and '"' + env['OKILUM_SIGNING_IDENTITY'] + '"' in line]
    if len(matches) != 1:
        raise ValueError('Imported private identity does not match frozen certificate/team')


def restore(keychains, default):
    call(['security', 'list-keychains', '-d', 'user', '-s', *keychains])
    if default:
        call(['security', 'default-keychain', '-d', 'user', '-s', default])
    actual = shlex.split(call(['security', 'list-keychains', '-d', 'user']).decode())
    current = shlex.split(call(['security', 'default-keychain', '-d', 'user']).decode())
    if actual != keychains or current != ([default] if default else []):
        raise ValueError('Original keychain configuration not restored')


def setup(env):
    payload = validate(env)  # All missing/context errors stop before any keychain writes.
    directory = root(env)
    directory.mkdir(mode=0o700)  # Existing/ambiguous operation directory is never reused.
    keychain = directory / 'shipping.keychain-db'
    password = secrets.token_urlsafe(32)
    baseline = {'search': shlex.split(call(['security', 'list-keychains', '-d', 'user']).decode()),
                'default': shlex.split(call(['security', 'default-keychain', '-d', 'user']).decode())}
    if len(baseline['default']) != 1:
        raise ValueError('Unknown original default keychain')
    (directory / 'baseline.json').write_text(json.dumps(baseline)); (directory / 'baseline.json').chmod(0o600)
    p12 = directory / 'identity.p12';p12.write_bytes(payload);p12.chmod(0o600)
    call(['security', 'create-keychain', '-p', password, str(keychain)])
    # create-keychain can add itself to the user list; immediately restore baseline.
    restore(baseline['search'], baseline['default'][0])
    call(['security', 'unlock-keychain', '-p', password, str(keychain)])
    call(['security', 'import', str(p12), '-k', str(keychain), '-P',
          env['OKILUM_MAC_DEVELOPER_ID_PASSWORD'], '-T', '/usr/bin/codesign'])
    call(['security', 'set-key-partition-list', '-S', 'apple-tool:,apple:,codesign:',
          '-s', '-k', password, str(keychain)])
    certificate_identity(call(['security', 'find-certificate', '-a', '-p', str(keychain)]).decode(),
                         call(['security', 'find-identity', '-v', '-p', 'codesigning', str(keychain)]).decode(), env)
    call(['xcrun', 'notarytool', 'store-credentials', env['OKILUM_NOTARY_PROFILE'],
          '--keychain', str(keychain), '--apple-id', env['OKILUM_MAC_NOTARY_APPLE_ID'],
          '--team-id', env['OKILUM_MAC_NOTARY_TEAM'], '--password', env['OKILUM_MAC_NOTARY_PASSWORD']])
    p12.unlink()
    with open(env['GITHUB_ENV'], 'a') as f:
        f.write('OKILUM_SIGNING_KEYCHAIN=' + str(keychain) + '\n')
    print('Ephemeral signing capability ready; no default/search-list changes')


def cleanup(env):
    directory = root(env)
    if directory.is_symlink():
        raise ValueError('Refuse aliased signing directory')
    if not directory.exists():
        return
    if directory.stat().st_uid != os.getuid() or not directory.is_dir():
        raise ValueError('Signing directory ownership mismatch')
    allowed = {'baseline.json', 'identity.p12', 'shipping.keychain-db', 'shipping.keychain-db-shm',
               'shipping.keychain-db-wal'}
    if any(p.name not in allowed or p.is_symlink() or not p.is_file()
           or p.stat().st_uid != os.getuid() for p in directory.iterdir()):
        raise ValueError('Unknown signing residue; preserve rather than remove unrelated files')
    baseline_file = directory / 'baseline.json'
    baseline = json.loads(baseline_file.read_text()) if baseline_file.exists() else None
    keychain = directory / 'shipping.keychain-db'
    if keychain.exists():
        call(['security', 'delete-keychain', str(keychain)])
    if baseline:
        restore(baseline['search'], baseline['default'][0])
    shutil.rmtree(directory)
    if directory.exists():
        raise ValueError('Signing cleanup incomplete')
    print('Ephemeral signing directory removed')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['setup', 'cleanup'])
    command = parser.parse_args().command
    try:
        {'setup': setup, 'cleanup': cleanup}[command](dict(os.environ))
    except Exception:
        # Commands can contain secret argv; no exception/traceback/response body is logged.
        print('Ephemeral signing ' + command + ' FAILED; no fallback or compilation', file=sys.stderr)
        sys.exit(1)
