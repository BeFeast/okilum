#!/usr/bin/env python3
"""Check every Windows program a release ships before anything is published (#1104).

    verify-signatures.py RELEASE_DIR [PORTABLE_ZIP]

RELEASE_DIR holds the Setup.exe and the full nupkg from `vpk pack`; every PE inside the
package (lib/app/*.exe, *.dll) and okilum.exe inside the portable ZIP are checked too.
Each one must verify with osslsigncode, carry a verified RFC 3161 timestamp, have exactly
one signature (Velopack on Linux re-signs what is not excluded, which would nest a second),
and be signed by the pinned certificate:
    OKILUM_WINDOWS_SIGN_CERT_SHA256  signer certificate SHA-256
    OKILUM_WINDOWS_SIGN_SUBJECT      its subject, RFC 2253 (optional)
    OKILUM_WINDOWS_SIGN_CA_FILE      trust anchors for the signer chain (default: system)
    OKILUM_WINDOWS_SIGN_TSA_CA_FILE  trust anchors for the timestamp (default: system)
"""
import glob
import hashlib
import os
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

SYSTEM_CA = '/etc/ssl/certs/ca-certificates.crt'
PE_SUFFIXES = ('.exe', '.dll')


def run(*args):
    return subprocess.run(args, capture_output=True, text=True)


def certificates(path, work):
    """SHA-256 and RFC 2253 subject of every certificate in the file's signature."""
    # Unique per call: the nupkg and the portable ZIP both ship an okilum.exe.
    signature = Path(tempfile.mkstemp(suffix='.p7', dir=work)[1])
    signature.unlink()
    if run('osslsigncode', 'extract-signature', '-in', str(path), '-out', str(signature)).returncode:
        return {}
    pem = run('openssl', 'pkcs7', '-inform', 'DER', '-in', str(signature), '-print_certs').stdout
    found = {}
    for block in re.findall(r'-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----', pem, re.S):
        der = subprocess.run(['openssl', 'x509', '-outform', 'DER'], input=block.encode(), capture_output=True).stdout
        subject = subprocess.run(['openssl', 'x509', '-noout', '-subject', '-nameopt', 'RFC2253'],
                                 input=block, capture_output=True, text=True).stdout
        found[hashlib.sha256(der).hexdigest()] = subject.strip().removeprefix('subject=')
    return found


def problems(path, work, pinned, subject, ca, tsa_ca):
    out = run('osslsigncode', 'verify', '-in', str(path), '-CAfile', ca, '-TSA-CAfile', tsa_ca)
    text = out.stdout + out.stderr
    issues = []
    if out.returncode or 'Signature verification: ok' not in text:
        issues.append('signature does not verify')
    if 'Timestamp Server Signature verification: ok' not in text:
        issues.append('no verified RFC 3161 timestamp')
    count = re.search(r'Number of verified signatures: (\d+)', text)
    if not count or count.group(1) != '1':
        issues.append(f"expected exactly one signature, found {count.group(1) if count else 'none'}")
    certs = certificates(path, work)
    if pinned not in certs:
        issues.append('not signed by the pinned certificate')
    elif subject and certs[pinned] != subject:
        issues.append(f'signer subject is {certs[pinned]!r}')
    return issues


def targets(release, portable, work):
    """(label, path) for every PE the release ships."""
    found = []
    # Velopack names the installer after the channel: BeFeast.Okilum-<channel>-Setup.exe.
    setups = glob.glob(str(release / '*Setup.exe'))
    if len(setups) != 1:
        sys.exit(f'verify-signatures: expected one Setup.exe in {release}, found {setups}')
    found.append((Path(setups[0]).name, Path(setups[0])))
    packages = glob.glob(str(release / '*-full.nupkg'))
    if len(packages) != 1:
        sys.exit(f'verify-signatures: expected one full nupkg in {release}, found {packages}')
    with zipfile.ZipFile(packages[0]) as package:
        for name in package.namelist():
            if name.lower().endswith(PE_SUFFIXES):
                target = work / 'nupkg' / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(package.read(name))
                found.append((f'nupkg:{name}', target))
    if portable:
        with zipfile.ZipFile(portable) as archive:
            for name in archive.namelist():
                if name.lower().endswith(PE_SUFFIXES):
                    target = work / 'portable' / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(archive.read(name))
                    found.append((f'portable:{name}', target))
    return found


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    release = Path(sys.argv[1])
    portable = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    pinned = os.environ['OKILUM_WINDOWS_SIGN_CERT_SHA256'].replace(':', '').strip().lower()
    subject = os.environ.get('OKILUM_WINDOWS_SIGN_SUBJECT', '').strip()
    ca = os.environ.get('OKILUM_WINDOWS_SIGN_CA_FILE') or SYSTEM_CA
    tsa_ca = os.environ.get('OKILUM_WINDOWS_SIGN_TSA_CA_FILE') or SYSTEM_CA
    failed = False
    with tempfile.TemporaryDirectory() as temporary:
        work = Path(temporary)
        for label, path in targets(release, portable, work):
            if not path.is_file():
                print(f'FAIL {label}: missing')
                failed = True
                continue
            issues = problems(path, work, pinned, subject, ca, tsa_ca)
            print(f"{'FAIL' if issues else 'ok  '} {label}" + (': ' + '; '.join(issues) if issues else ''))
            failed |= bool(issues)
    if failed:
        sys.exit('verify-signatures: a shipped program is not correctly signed; nothing is published')


if __name__ == '__main__':
    main()
