import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('ephemeral_macos_signer', Path(__file__).with_name('macos-keychain.py'))
m = importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
DER = b'public certificate fixture'
PEM = '-----BEGIN CERTIFICATE-----\n' + base64.b64encode(DER).decode() + '\n-----END CERTIFICATE-----'
IDENTITY = 'Developer ID Application: Test (ABCDEFGHIJ)'


class EphemeralSigning(unittest.TestCase):
    def env(self, tmp):
        return {'RUNNER_TEMP': tmp, 'GITHUB_RUN_ID': '700', 'GITHUB_RUN_ATTEMPT': '1',
                'GITHUB_REF': 'refs/heads/main', 'GITHUB_REPOSITORY': 'BeFeast/okilum',
                'PRODUCT_SOURCE': 'a' * 40, 'GITHUB_SHA': 'b' * 40,
                'OKILUM_MAC_DEVELOPER_ID_P12': base64.b64encode(b'private fixture').decode(),
                'OKILUM_MAC_DEVELOPER_ID_PASSWORD': 'fixture-pass', 'OKILUM_MAC_NOTARY_APPLE_ID': 'test@example.invalid',
                'OKILUM_MAC_NOTARY_PASSWORD': 'fixture-notary', 'OKILUM_MAC_NOTARY_TEAM': 'ABCDEFGHIJ',
                'OKILUM_MAC_CERT_SHA256': hashlib.sha256(DER).hexdigest(),
                'OKILUM_SIGNING_IDENTITY': IDENTITY, 'OKILUM_NOTARY_PROFILE': 'profile',
                'GITHUB_ENV': str(Path(tmp) / 'env')}

    def backend(self, env, commands, fail=None):
        state = {'search': ['/original/login.keychain-db'], 'default': '/original/login.keychain-db'}
        def execute(argv, **kwargs):
            commands.append(argv)
            if fail and fail in argv:
                raise subprocess.CalledProcessError(1, argv)
            if argv[:3] == ['git', 'rev-parse', 'HEAD']:return (env['GITHUB_SHA'] + '\n').encode()
            if argv[0] == 'uname':return b'Darwin arm64\n'
            if argv[:2] == ['security', 'list-keychains']:
                if '-s' in argv:state['search'] = argv[5:];return b''
                return (' '.join(json.dumps(x) for x in state['search'])).encode()
            if argv[:2] == ['security', 'default-keychain']:
                if '-s' in argv:state['default'] = argv[-1];return b''
                return json.dumps(state['default']).encode()
            if argv[:2] == ['security', 'create-keychain']:
                Path(argv[-1]).write_bytes(b'private-keychain-fixture')
                state['search'].append(argv[-1])  # Actual API side-effect model must be restored.
            if argv[:2] == ['security', 'find-certificate']:return PEM.encode()
            if argv[:2] == ['security', 'find-identity']:
                return ('1) ' + hashlib.sha1(DER).hexdigest().upper() + ' "' + IDENTITY + '"').encode()
            if argv[:2] == ['security', 'delete-keychain']:Path(argv[-1]).unlink()
            return b''
        return execute, state

    def test_setup_pinned_identity_explicit_keychain_then_cleanup_restores_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = self.env(tmp);commands=[];backend,state=self.backend(env,commands)
            with patch.object(m, 'call', side_effect=backend):
                m.setup(env);directory=m.root(env)
                self.assertEqual(state['search'], ['/original/login.keychain-db'])
                self.assertFalse((directory/'identity.p12').exists())
                self.assertIn('OKILUM_SIGNING_KEYCHAIN='+str(directory/'shipping.keychain-db'), Path(env['GITHUB_ENV']).read_text())
                for argv in commands:
                    if 'store-credentials' in argv:
                        self.assertEqual(argv[argv.index('--keychain')+1], str(directory/'shipping.keychain-db'))
                m.cleanup(env)
                self.assertFalse(directory.exists())
                self.assertEqual(state, {'search':['/original/login.keychain-db'],'default':'/original/login.keychain-db'})

    def test_missing_credentials_ref_attempt_or_team_never_create_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            for changes in [{'OKILUM_MAC_DEVELOPER_ID_P12':''}, {'GITHUB_REF':'refs/pull/1/head'},
                            {'GITHUB_RUN_ATTEMPT':'2'}, {'OKILUM_MAC_CERT_SHA256':'bad'},
                            {'OKILUM_MAC_NOTARY_TEAM':'ZZZZZZZZZZ'}]:
                env=self.env(tmp)|changes;commands=[];backend,_=self.backend(env,commands)
                with patch.object(m,'call',side_effect=backend),self.assertRaises(ValueError):m.setup(env)
                self.assertFalse(any(x.name.startswith('okilum-signing-') for x in Path(tmp).iterdir()))
                self.assertFalse(any('create-keychain' in x for x in commands))

    def test_failed_import_or_notary_cleans_only_partial_own_state(self):
        for fail in ['import','store-credentials']:
            with self.subTest(fail=fail),tempfile.TemporaryDirectory() as tmp:
                env=self.env(tmp);commands=[];backend,_=self.backend(env,commands,fail)
                with patch.object(m,'call',side_effect=backend),self.assertRaises(subprocess.CalledProcessError):m.setup(env)
                backend,state=self.backend(env,commands)
                with patch.object(m,'call',side_effect=backend):m.cleanup(env)
                self.assertFalse(m.root(env).exists())
                self.assertFalse(Path(env['GITHUB_ENV']).exists())

    def test_certificate_mismatch_blocks_profile_and_aliased_cleanup_refuses(self):
        with tempfile.TemporaryDirectory() as tmp:
            env=self.env(tmp)|{'OKILUM_MAC_CERT_SHA256':'0'*64};commands=[];backend,_=self.backend(env,commands)
            with patch.object(m,'call',side_effect=backend),self.assertRaises(ValueError):m.setup(env)
            self.assertFalse(any('store-credentials' in x for x in commands))
        with tempfile.TemporaryDirectory() as tmp:
            env=self.env(tmp);outside=Path(tmp)/'outside';outside.mkdir();(outside/'precious').write_text('keep')
            m.root(env).symlink_to(outside,target_is_directory=True)
            with self.assertRaises(ValueError):m.cleanup(env)
            self.assertEqual((outside/'precious').read_text(),'keep')

    def test_explicit_keychain_is_used_for_all_signers_and_notary(self):
        repo=Path(__file__).resolve().parents[2]
        for file in ['scripts/updater/sign-bundle.sh','scripts/updater/make-dmg.sh']:
            text=(repo/file).read_text()
            self.assertIn('codesign ${SIGNING_KEYCHAIN_ARGS[@]+"${SIGNING_KEYCHAIN_ARGS[@]}"} --force',text)
            self.assertIn('${NOTARY_KEYCHAIN_ARGS[@]+"${NOTARY_KEYCHAIN_ARGS[@]}"} --wait',text)
        self.assertIn('codesign ${keychain_args[@]+"${keychain_args[@]}"} --force',(repo/'scripts/ci/supervisor-bundle.sh').read_text())


if __name__=='__main__':unittest.main()
