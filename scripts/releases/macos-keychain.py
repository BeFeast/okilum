#!/usr/bin/env python3
"""Opt-in ephemeral hosted signer. Restores the original search list/default after the job."""
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
    if env.get('VERIFY_ONLY') == 'true':
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        from macos_verify_authorization import authorize
        if authorize(env) != source:
            raise ValueError('Verify source differs from signing source')
    else:
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
    # codesign's private-key lookup needs the keychain in the search list even
    # with --keychain. Keep it registered for this disposable job, never default.
    restore([str(keychain), *baseline['search']], baseline['default'][0])
    # A fresh keychain may auto-lock while Cargo builds. Scope a two-hour
    # timeout to this disposable job keychain (shipping job itself is 90 min),
    # never the user's login/default keychain. Keep normal lock-on-sleep.
    call(['security', 'set-keychain-settings', '-lut', '7200', str(keychain)])
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
    # Exercise private-key access before spending time compiling. Identity listing
    # and Notary history alone do not prove codesign can retrieve the private key.
    probe = directory / 'signing-probe'
    shutil.copyfile('/usr/bin/true', probe)
    call(['codesign', '--keychain', str(keychain), '--force', '--timestamp',
          '--sign', env['OKILUM_SIGNING_IDENTITY'], str(probe)])
    call(['codesign', '--verify', '--strict', str(probe)])
    probe.unlink()
    p12.unlink()
    with open(env['GITHUB_ENV'], 'a') as f:
        f.write('OKILUM_SIGNING_KEYCHAIN=' + str(keychain) + '\n')
    print('Ephemeral signing capability ready; private-key signing verified; default unchanged')


def cleanup(env):
    directory = root(env)
    if directory.is_symlink():
        raise ValueError('Refuse aliased signing directory')
    if not directory.exists():
        return
    if directory.stat().st_uid != os.getuid() or not directory.is_dir():
        raise ValueError('Signing directory ownership mismatch')
    # This directory was exclusively created by this attempt with mode 0700.
    # Security.framework may create additional regular auxiliary files. Their
    # names are not a stable API; never traverse aliases or nested directories.
    if directory.stat().st_mode & 0o077:
        raise ValueError('Signing directory is not private')
    if any(p.is_symlink() or not p.is_file()
           or p.stat().st_uid != os.getuid() for p in directory.iterdir()):
        raise ValueError('Unsafe signing residue; refusing traversal')
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


NOTARY_REASONS = [('agreement', 'required Apple agreement missing or expired'),
                  ('invalid credentials', 'invalid Apple ID or app-specific password'),
                  ('username or password', 'invalid Apple ID or app-specific password'),
                  ('locked', 'Apple account locked'),
                  ('team', 'team not available for this Apple ID')]


def notary_reason(stderr):
    """Fixed, secret-free diagnosis of a notarytool failure: only the HTTP status
    number and one of the known reason labels above, never the raw output."""
    text = (stderr or b'').decode('utf-8', 'replace').lower()
    status = re.search(r'http status code: ([0-9]{3})', text)
    reason = next((label for key, label in NOTARY_REASONS if key in text), 'unrecognized reason')
    return ('HTTP ' + status[1] + ' ' if status else 'exit ') + reason

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['setup', 'cleanup'])
    command = parser.parse_args().command
    try:
        {'setup': setup, 'cleanup': cleanup}[command](dict(os.environ))
    except Exception as error:
        # Commands can contain secret argv; no exception/traceback/response body is logged.
        operation = type(error).__name__
        if isinstance(error, ValueError):
            operation += ':' + str(error)  # Only fixed validation messages, no secret inputs.
        if isinstance(error, subprocess.CalledProcessError):
            # Only fixed executable/subcommand names, never arguments or output.
            operation += ':' + Path(error.cmd[0]).name
            if error.cmd[0] == 'security':
                operation += ':' + error.cmd[1]
            if error.cmd[:2] == ['xcrun', 'notarytool']:
                operation += ':' + notary_reason(error.stderr)
        print('Ephemeral signing ' + command + ' FAILED (' + operation + ')', file=sys.stderr)
        sys.exit(1)
