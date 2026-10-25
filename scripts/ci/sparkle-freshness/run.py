#!/usr/bin/env python3
"""Hosted macOS only: real signed Sparkle downloads, cancellation and installation.

No production signing keys, feed overrides, vendor patches or bypassed validation.
Each run owns isolated app identities, bundles, ephemeral test keys and a local feed.
"""
import datetime
import email.utils
import errno
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


def remove_case(case, *, timeout=10, remove=shutil.rmtree,
                clock=time.monotonic, sleep=time.sleep):
    """Allow late fixture-helper writes to settle; never hide failed cleanup."""
    deadline = clock() + timeout
    while True:
        try:
            remove(case)
            return
        except OSError as error:
            if error.errno != errno.ENOTEMPTY:
                raise
            remaining = deadline - clock()
            if remaining <= 0:
                raise
            sleep(min(0.1, remaining))


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
    trace = log.with_suffix('.trace')
    callbacks = trace.read_text() if trace.exists() else '(no callback trace)'
    raise AssertionError(f'Timeout: {log.read_text()}\nCallbacks:\n{callbacks}')


UPDATER_START_BOUNDARY = 'window probe positive control detected standard Sparkle check'


def assert_rollout_ordering(callbacks, group_receipt, probe_receipt):
    # ArmWindowProbe records this positive control immediately before TSStart.
    # A cancelled check can finish without ever offering an update (report 1).
    assert UPDATER_START_BOUNDARY in callbacks, callbacks
    for receipt in (group_receipt, probe_receipt):
        assert receipt in callbacks, callbacks
        assert callbacks.index(receipt) < callbacks.index(UPDATER_START_BOUNDARY), callbacks


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
        guard = work / 'libfixture_guard.a'
        run('rustc', '--edition=2021', '--crate-type=staticlib',
            ROOT / 'scripts/ci/sparkle-freshness/guard.rs', '-o', guard)
        executable = work / 'Freshness'
        run('xcrun', 'clang', '-fobjc-arc', '-fblocks', '-Wall', '-Werror',
            '-Wno-unused-parameter',
            '-F', FRAMEWORK, '-framework', 'Sparkle', '-framework', 'AppKit',
            '-Wl,-rpath,@executable_path/../Frameworks',
            ROOT / 'scripts/ci/sparkle-freshness/host.m', guard, '-liconv', '-o', executable)
        for scenario in ('termination-refused-retry', 'termination-delayed-retry', 'termination-retry-guard-refusal', 'discovery', 'cancel-verifying', 'cancel-check', 'cancel-download', 'cancel-ready', 'newest', 'background-newest', 'background-phase-unchanged', 'background-phase-included', 'restart-newest', 'quit-newest', 'quit-during-download', 'unchanged', 'stable', 'bad-signature', 'guard-refusal', 'quit-guard-refusal', 'phase-unchanged', 'older-feed', 'feed-error',
                         'eligibility-phase-background', 'eligibility-phase-foreground',
                         'eligibility-skip-background', 'eligibility-skip-foreground',
                         'eligibility-major-background', 'eligibility-major-foreground',
                         'eligibility-delta-foreground', 'eligibility-os-background'):
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
            compare = scenario.startswith('eligibility-')
            foreground = scenario.endswith('-foreground')
            base_info = {
                'CFBundleIdentifier': identifier, 'CFBundleExecutable': 'Freshness',
                'CFBundleName': 'Freshness', 'CFBundlePackageType': 'APPL',
                'SUFeedURL': address + '/appcast.xml', 'SUPublicEDKey': public_key,
                'SUEnableAutomaticChecks': True, 'SUAllowsAutomaticUpdates': False,
                'SUVerifyUpdateBeforeExtraction': True, 'SURequireSignedFeed': True,
                'NSAppTransportSecurity': {'NSAllowsLocalNetworking': True},
                'TSTerminationRetry': 'delay' if scenario == 'termination-delayed-retry' else 'refuse' if scenario.startswith('termination-') else '',
                'TSRetryGuardRefuse': scenario == 'termination-retry-guard-refusal',
                'TSCancel': scenario.removeprefix('cancel-') if scenario.startswith('cancel-') else '',
                'TSTrace': str(trace), 'TSReceipt': str(receipt),
                'TSPendingFeed': str(case / 'pending.xml'), 'TSFeedPath': str(case / 'appcast.xml'),
                'TSBeta': scenario != 'stable', 'TSRefuse': scenario in ('guard-refusal', 'quit-guard-refusal'),
                'TSEditor': str(case / 'editor'), 'TSProtectedEditor': str(case / 'protected-editor'),
                'TSCompareEligibility': compare, 'TSForeground': foreground,
                'TSPhaseGroup': 0 if scenario == 'background-phase-included' else 6,
                'TSBackgroundInstall': scenario.startswith('background-') or scenario == 'discovery',
                'TSDiscovery': scenario == 'discovery',
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

            (case / 'editor').write_text('saved before verification')
            if compare:
                base_info['SUDefaultsDomain'] = identifier + '.updates'
                base_info['TSExpectDelta'] = 'delta' in scenario
                base_info['TSExpectedSelection'] = '4' if foreground else '2' if ('phase' in scenario or 'os' in scenario) else 'none'
                if 'skip' in scenario:
                    base_info['TSSkip'] = '4'
                if 'major' in scenario:
                    base_info['TSSkipMajor'] = '2'
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
                    context = ''
                    if ('phase' in scenario and (version == 4 or scenario == 'phase-unchanged')):
                        released = email.utils.format_datetime(datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=60))
                        context += f'<pubDate>{released}</pubDate><sparkle:phasedRolloutInterval>604800</sparkle:phasedRolloutInterval>'
                    if compare and 'major' in scenario:
                        context += '<sparkle:minimumAutoupdateVersion>2</sparkle:minimumAutoupdateVersion>'
                    if compare and 'os' in scenario and version == 4:
                        context += '<sparkle:minimumSystemVersion>99.0</sparkle:minimumSystemVersion>'
                    if compare and 'delta' in scenario and version == 4:
                        (case / 'delta4.zip').write_bytes((case / '4.zip').read_bytes())
                        context += f'<sparkle:deltas><enclosure url="{address}/delta4.zip" sparkle:version="4" sparkle:deltaFrom="1" sparkle:edSignature="{signature}" length="{(case / "4.zip").stat().st_size}" type="application/octet-stream"/></sparkle:deltas>'
                    items.append(f'''<item><title>{version}</title><sparkle:channel>{channel}</sparkle:channel>{context}
<enclosure url="{address}/{version}.zip" sparkle:version="{version}"
 sparkle:shortVersionString="0.1.{version}" sparkle:edSignature="{signature}"
 length="{(case / f'{version}.zip').stat().st_size}" type="application/octet-stream"/></item>''')
                path.write_text('<?xml version="1.0"?><rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><title>Freshness test</title>' + ''.join(items) + '</channel></rss>')
                run(signer, '--ed-key-file', key, path, stdout=subprocess.DEVNULL)

            feed(case / 'appcast.xml', [(4, 'stable'), (2, 'stable')] if compare else [(2, 'stable')])
            newer = [(4, 'beta'), (3, 'stable'), (2, 'stable')]
            feed(case / 'pending.xml', [(2, 'stable')] if scenario in ('unchanged', 'phase-unchanged', 'guard-refusal', 'quit-guard-refusal', 'restart-newest', 'quit-newest') else newer)
            if scenario == 'background-phase-unchanged':
                feed(case / 'pending.xml', [(4, 'beta'), (2, 'stable')])
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
                    group = base_info['TSPhaseGroup']
                    domain = base_info.get('SUDefaultsDomain', 'standard-main-bundle')
                    group_receipt = f'rollout group read={group} domain={domain} before-updater-start'
                    probe_domain = base_info.get('SUDefaultsDomain', identifier)
                    probe_receipt = f'rollout probe group read={group} domain={probe_domain} matches-source before-updater-start'
                    wait_for(lambda: UPDATER_START_BOUNDARY in trace.read_text(), process, output)
                    callbacks = trace.read_text()
                    assert_rollout_ordering(callbacks, group_receipt, probe_receipt)
                    expected = '2' if scenario in ('unchanged', 'phase-unchanged', 'background-phase-unchanged') else '3' if scenario == 'stable' else '4'
                    if scenario == 'termination-retry-guard-refusal':
                        wait_for(lambda: 'retry editor guard refused' in trace.read_text(), process, output)
                        time.sleep(3)
                        assert process.poll() is None, trace.read_text()
                        assert not receipt.exists()
                        assert plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion'] == '1'
                        assert trace.read_text().count('termination requested') == 1, trace.read_text()
                        assert 'About retry enabled after original delegate decision' in trace.read_text()
                        assert (case / 'protected-editor').read_text() == 'edited after verification'
                        print('PASS retry editor refusal preserves app and About action', flush=True)
                        continue
                    if scenario == 'discovery':
                        wait_for(lambda: process.poll() is not None, process, output)
                        assert process.returncode == 0, trace.read_text()
                        assert 'report 1 ' in trace.read_text(), trace.read_text()
                        assert not any('.zip' in r for r in requests), requests
                        assert not receipt.exists()
                        assert plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion'] == '1'
                        print('PASS automatic discovery requires About Install intent', flush=True)
                        continue
                    if scenario.startswith('cancel-'):
                        wait_for(lambda: process.poll() is not None, process, output)
                        time.sleep(3)  # allow any mistakenly retained installer to run
                        assert process.returncode == 0, trace.read_text()
                        assert 'report 7 ' in trace.read_text(), trace.read_text()
                        assert 'ordinary quit after cancellation' in trace.read_text(), trace.read_text()
                        assert not receipt.exists(), trace.read_text()
                        assert plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion'] == '1'
                        assert 'final editor guard boundary' not in trace.read_text()
                        print(f'PASS Sparkle cancellation: {scenario}', flush=True)
                        continue
                    if compare:
                        wait_for(receipt.exists, process, output)
                        assert receipt.read_text() == 'eligibility matched', trace.read_text()
                        assert 'eligibility matches' in trace.read_text(), trace.read_text()
                        assert not any('.zip' in r for r in requests), requests
                        print(f'PASS Sparkle source/probe eligibility: {scenario}', flush=True)
                        continue
                    if scenario in ('bad-signature', 'guard-refusal', 'quit-guard-refusal', 'older-feed', 'feed-error'):
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
                            assert trace.read_text().count('termination requested') == 1, trace.read_text()
                            assert trace.read_text().index('protect current editor after verification') < trace.read_text().index('termination requested'), trace.read_text()
                            if scenario != 'quit-during-download':
                                assert 'ordinary quit returned to main queue' in trace.read_text(), trace.read_text()
                        else:
                            wait_for(receipt.exists, process, output)
                            assert receipt.read_text() == expected, trace.read_text()
                        installed = plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleVersion']
                        assert installed == expected, (installed, trace.read_text())
                        assert 'final editor guard boundary' in trace.read_text()
                        expected_revision = 'edited after retry verification' if scenario.startswith('termination-') else 'edited after verification'
                        assert (case / 'protected-editor').read_text() == expected_revision, trace.read_text()
                        assert trace.read_text().index('editor changed after verification') < trace.read_text().index('protect current editor after verification'), trace.read_text()
                        if scenario != 'quit-during-download':
                            assert f'verified-ready {expected}' in trace.read_text()
                        assert trace.read_text().count('original delegate restored for shutdown') == 1, trace.read_text()
                        if scenario not in ('unchanged', 'phase-unchanged', 'background-phase-unchanged'):
                            assert 'replacement session started before scheduling' in trace.read_text(), trace.read_text()
                            assert trace.read_text().index('dismiss 2') < trace.read_text().index(f'offer {expected}'), trace.read_text()
                            assert any(f'GET /{expected}.zip' in r for r in requests), requests
                        if scenario not in ('unchanged', 'phase-unchanged', 'background-phase-unchanged', 'restart-newest', 'quit-newest'):
                            assert 'verified-ready 2' not in trace.read_text(), trace.read_text()
                    if scenario.startswith('termination-'):
                        callbacks = trace.read_text()
                        decision = ('original delegate resolved delayed termination NO' if scenario == 'termination-delayed-retry'
                                    else 'original delegate refused first termination')
                        assert callbacks.index(decision) < callbacks.index('About retry enabled after original delegate decision')
                        assert callbacks.index('deliberate About retry requested') < callbacks.index('retry editor guard committed')
                        assert callbacks.index('retry editor guard committed') < callbacks.index('original delegate accepted deliberate retry')
                        assert callbacks.count('termination requested') == 2, callbacks
                        assert callbacks.count('final editor guard boundary') == 2, callbacks
                        assert callbacks.count('protect current editor after verification') == 2, callbacks
                        assert callbacks.count('persist UI after editor protection') == 2, callbacks
                        assert sum('GET /appcast.xml' in r for r in requests) >= 5, requests
                    if scenario in ('guard-refusal', 'quit-guard-refusal'):
                        assert 'editor changed after verification' in trace.read_text()
                        assert 'protect current editor after verification' in trace.read_text()
                        assert 'persist UI after editor protection' not in trace.read_text()
                        assert not (case / 'protected-editor').exists()
                        assert 'termination requested' not in trace.read_text()
                    if scenario.startswith('background-'):
                        assert 'selection background' in trace.read_text()
                    if scenario == 'background-phase-unchanged':
                        assert not any('GET /4.zip' in r for r in requests), requests
                    if scenario == 'background-phase-included':
                        assert any('GET /4.zip' in r for r in requests), requests
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
                    if compare:
                        subprocess.run(['defaults', 'delete', identifier + '.updates'], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    # The host is reaped and the HTTP server is stopped above.
                    # Sparkle helpers may finish a final filesystem write after
                    # that boundary. Retry only ENOTEMPTY, with a hard deadline.
                    remove_case(case)
                    assert 'window probe positive control detected standard Sparkle check' in trace.read_text(), trace.read_text()
                    assert 'unexpected updater window' not in trace.read_text(), trace.read_text()


if __name__ == '__main__':
    main()
