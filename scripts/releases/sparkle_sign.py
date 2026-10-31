"""Forgejo-side standard Ed25519 Sparkle signing; no key leaves this runner."""
import base64
import os
from pathlib import Path
import subprocess
import tempfile

PUBLIC_KEY = 'TH/Jc6vRfSSFtR3R2rLqBGBnFylMUWdKJ6dr18Ys8CY='


def sign_archive(archive, encoded, public_key=PUBLIC_KEY):
    # Sparkle 2.10 common_cli/Secret.swift: current secret is the 32-byte
    # Ed25519 seed. The old 96-byte expanded-key format is NOT a standard seed.
    seed = base64.b64decode(encoded.strip(), validate=True)
    public = base64.b64decode(public_key, validate=True)
    if len(seed) != 32 or len(public) != 32:
        raise ValueError('Unsupported Sparkle key format; require existing native sign_update route')
    with tempfile.TemporaryDirectory(prefix='sparkle-sign-') as temp:
        root = Path(temp)
        private, pub, signature = (root / n for n in ['private.der', 'public.der', 'signature'])
        private.write_bytes(bytes.fromhex('302e020100300506032b657004220420') + seed)
        private.chmod(0o600)
        def run(*args):
            return subprocess.run(['openssl', *args], check=True, capture_output=True)
        try:
            run('pkey', '-inform', 'DER', '-in', str(private), '-pubout', '-outform', 'DER', '-out', str(pub))
            if pub.read_bytes() != bytes.fromhex('302a300506032b6570032100') + public:
                raise ValueError('Sparkle key does not match the installed public key')
            run('pkeyutl', '-sign', '-rawin', '-keyform', 'DER', '-inkey', str(private),
                '-in', str(archive), '-out', str(signature))
            value = signature.read_bytes()
            if len(value) != 64:
                raise ValueError('Invalid Ed25519 signature size')
            run('pkeyutl', '-verify', '-rawin', '-pubin', '-keyform', 'DER', '-inkey', str(pub),
                '-in', str(archive), '-sigfile', str(signature))
            return base64.b64encode(value).decode('ascii')
        except subprocess.CalledProcessError as error:
            # Neither OpenSSL diagnostics nor key bytes belong in public logs.
            raise ValueError('Sparkle signing/verification failed') from None
