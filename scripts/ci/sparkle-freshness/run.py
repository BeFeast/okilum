#!/usr/bin/env python3
"""Hosted macOS only: real signed Sparkle downloads, cancellation and installation.

No production signing keys, feed overrides, vendor patches or bypassed validation.
Each run owns isolated app identities, bundles, ephemeral test keys and a local feed.
"""
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[3]
FRAMEWORK = ROOT / 'vendor/sparkle'


def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, **kwargs)


def wait_for(predicate, process, log, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        if process.poll() not in (None, 0):
            raise AssertionError(f'Host exited {process.returncode}: {log.read_text()}')
        time.sleep(.1)
    raise AssertionError(f'Timeout: {log.read_text()}')


def main():
    assert sys.platform == 'darwin' and os.environ.get('GITHUB_SERVER_URL') == 'https://github.com', 'This integration test is hosted-macOS only'
    archive = Path(sys.argv[1])
    lock = json.loads((ROOT / 'scripts/updater/sparkle-lock.json').read_text())
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == lock['archive_sha256']
    evidence = Path(os.environ['RUNNER_TEMP']) / 'sparkle-freshness-evidence'
    evidence.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='okilum-sparkle-') as temporary:
        work = Path(temporary)
        signer = work / 'sign_update'
        with tarfile.open(archive) as package:
            member = next(m for m in package if m.name.removeprefix('./') == 'bin/sign_update')
            signer.write_bytes(package.extractfile(member).read())
        signer.chmod(0o700)
        # Sparkle's documented 32-byte seed format, generated in
        # memory by CryptoKit. Never accesses an existing keychain or credential.
        keygen = work / 'keys.swift'
        keygen.write_text('''import Foundation
import CryptoKit
let key = Curve25519.Signing.PrivateKey()
let directory = URL(fileURLWithPath: CommandLine.arguments[1])
try key.rawRepresentation.base64EncodedData().write(to: directory.appendingPathComponent("test-key"))
try key.publicKey.rawRepresentation.base64EncodedData().write(to: directory.appendingPathComponent("public-key"))
''')
        run('swift', keygen, work)
        key = work / 'test-key'
        key.chmod(0o600)
        public_key = (work / 'public-key').read_text()
        executable = work / 'Freshness'
        run('xcrun', 'clang', '-fobjc-arc', '-fblocks', '-Wall', '-Werror',
            '-Wno-unused-parameter',
            '-F', FRAMEWORK, '-framework', 'Sparkle', '-framework', 'AppKit',
            '-Wl,-rpath,@executable_path/../Frameworks',
            ROOT / 'scripts/ci/sparkle-freshness/host.m', '-o', executable)
        for scenario in ('newest', 'restart-newest', 'quit-newest', 'quit-during-download', 'unchanged', 'stable', 'bad-signature', 'guard-refusal', 'older-feed', 'feed-error'):
            case = work / scenario
            case.mkdir()
            trace = evidence / f'{scenario}.trace'
            trace.write_text('')
            output = evidence / f'{scenario}.log'
            requests = []

            class Handler(http.server.SimpleHTTPRequestHandler):
                def log_message(self, fmt, *args):
                    requests.append(fmt % args)

                def end_headers(self):
                    self.send_header('Cache-Control', 'no-store')
                    super().end_headers()

            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(case)))
            threading.Thread(target=server.serve_forever, daemon=True).start()
            address = f'http://127.0.0.1:{server.server_port}'
            identifier = 'uk.oklabs.okilum.freshness.' + scenario + '.' + os.urandom(6).hex()
            receipt = case / 'installed'
            app = case / 'Freshness.app'
            base_info = {
                'CFBundleIdentifier': identifier, 'CFBundleExecutable': 'Freshness',
                'CFBundleName': 'Freshness', 'CFBundlePackageType': 'APPL',
                'SUFeedURL': address + '/appcast.xml', 'SUPublicEDKey': public_key,
                'SUEnableAutomaticChecks': True, 'SUAllowsAutomaticUpdates': False,
                'SUVerifyUpdateBeforeExtraction': True, 'SURequireSignedFeed': True,
                'NSAppTransportSecurity': {'NSAllowsLocalNetworking': True},
                'TSTrace': str(trace), 'TSReceipt': str(receipt),
                'TSPendingFeed': str(case / 'pending.xml'), 'TSFeedPath': str(case / 'appcast.xml'),
                'TSBeta': scenario != 'stable', 'TSRefuse': scenario == 'guard-refusal',
                'TSQuit': scenario.startswith('quit-'),
                'TSQuitDuringDownload': scenario == 'quit-during-download',
            }

            def bundle(destination, version):
                (destination / 'Contents/MacOS').mkdir(parents=True)
                shutil.copy2(executable, destination / 'Contents/MacOS/Freshness')
                run('ditto', FRAMEWORK / 'Sparkle.framework', destination / 'Contents/Frameworks/Sparkle.framework')
                info = dict(base_info, CFBundleVersion=str(version), CFBundleShortVersionString=f'0.1.{version}')
                (destination / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
                run('codesign', '--force', '--sign', '-', destination, stdout=subprocess.DEVNULL)

            if scenario in ('restart-newest', 'quit-newest'):
                base_info['TSRestartFeed'] = str(case / 'restart.xml')
            signatures = {}
            for version in ('1.5', 2, 3, 4):
                stage = case / f'stage-{version}' / 'Freshness.app'
                bundle(stage, version)
                zip_path = case / f'{version}.zip'
                run('ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', stage, zip_path)
                signatures[version] = run(signer, '--ed-key-file', key, '-p', zip_path,
                                          capture_output=True, text=True).stdout.strip()
                shutil.rmtree(stage.parent)

            def feed(path, versions):
                items = []
                for version, channel in versions:
                    signature = signatures[version]
                    if scenario == 'bad-signature' and version == 4:
                        signature = 'A' * 86 + '=='
                    items.append(f'''<item><title>{version}</title><sparkle:channel>{channel}</sparkle:channel>
<enclosure url="{address}/{version}.zip" sparkle:version="{version}"
 sparkle:shortVersionString="0.1.{version}" sparkle:edSignature="{signature}"
 length="{(case / f'{version}.zip').stat().st_size}" type="application/octet-stream"/></item>''')
                path.write_text('<?xml version="1.0"?><rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><title>Freshness test</title>' + ''.join(items) + '</channel></rss>')
                run(signer, '--ed-key-file', key, path, stdout=subprocess.DEVNULL)

            feed(case / 'appcast.xml', [(2, 'stable')])
            newer = [(4, 'beta'), (3, 'stable'), (2, 'stable')]
            feed(case / 'pending.xml', [(2, 'stable')] if scenario in ('unchanged', 'guard-refusal', 'restart-newest', 'quit-newest') else newer)
            if scenario in ('restart-newest', 'quit-newest'):
                feed(case / 'restart.xml', newer)
            if scenario == 'older-feed':
                # Still newer than the running app, but older than downloaded N.
                feed(case / 'pending.xml', [('1.5', 'stable')])
            if scenario == 'feed-error':
                (case / 'pending.xml').write_text('not an appcast')
            bundle(app, 1)
            with output.open('w') as log_file:
                environment = dict(os.environ)
                # Exercise the app's embedded framework, as a shipped app does,
                # rather than the surrounding Rust gate's framework search path.
                environment.pop('DYLD_FRAMEWORK_PATH', None)
                process = subprocess.Popen([str(app / 'Contents/MacOS/Freshness')], stdout=log_file, stderr=log_file, env=environment)
                try:
                    expected = '2' if scenario == 'unchanged' else '3' if scenario == 'stable' else '4'
                    if scenario in ('bad-signature', 'guard-refusal', 'older-feed', 'feed-error'):
                        wait_for(lambda: 'report 2 ' in trace.read_text(), process, output)
                        assert not receipt.exists(), trace.read_text()
                        assert plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion'] == '1'
                    else:
                        def installed_version():
                            try:
                                return plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion']
                            except (FileNotFoundError, plistlib.InvalidFileException):
                                return None
                        if scenario.startswith('quit-'):
                            wait_for(lambda: installed_version() == expected, process, output)
                            assert not receipt.exists(), 'Ordinary Quit must not request relaunch'
                        else:
                            wait_for(receipt.exists, process, output)
                            assert receipt.read_text() == expected, trace.read_text()
                        installed = plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion']
                        assert installed == expected, (installed, trace.read_text())
                        if not scenario.startswith('quit-'):
                            assert 'final editor guard boundary' in trace.read_text()
                        if scenario != 'quit-during-download':
                            assert f'verified-ready {expected}' in trace.read_text()
                        assert trace.read_text().count('original delegate restored for shutdown') == 1, trace.read_text()
                        if scenario != 'unchanged':
                            assert trace.read_text().index('dismiss 2') < trace.read_text().index(f'offer {expected}'), trace.read_text()
                            assert any(f'GET /{expected}.zip' in r for r in requests), requests
                        if scenario not in ('unchanged', 'restart-newest', 'quit-newest'):
                            assert 'verified-ready 2' not in trace.read_text(), trace.read_text()
                    assert 'downloaded 2' in trace.read_text(), trace.read_text()
                    assert 'prepared 2' in trace.read_text(), trace.read_text()
                    assert 'published feed' in trace.read_text(), trace.read_text()
                    assert sum('GET /appcast.xml' in r for r in requests) >= 2, requests
                    print(f'PASS Sparkle real lifecycle: {scenario}', flush=True)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                    server.shutdown()
                    server.server_close()
                    (evidence / f'{scenario}.requests.json').write_text(json.dumps(requests, indent=2))
                    info_path = app / 'Contents/Info.plist'
                    if info_path.exists():
                        info = plistlib.loads(info_path.read_bytes())
                        (evidence / f'{scenario}.installed.json').write_text(json.dumps({
                            'bundleVersion': info['CFBundleVersion'],
                            'relaunchReceipt': receipt.read_text() if receipt.exists() else None,
                        }, indent=2))
                    # Only this throwaway test identity, never the user's defaults.
                    subprocess.run(['defaults', 'delete', identifier], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    shutil.rmtree(case)


if __name__ == '__main__':
    main()
