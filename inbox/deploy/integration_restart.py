#!/usr/bin/env python3
"""Disposable Docker acceptance; no live deployment access or credentials."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.error

import restart


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def fingerprint(path):
    with closing(sqlite3.connect(path)) as db:
        tables = db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").fetchall()
        rows = {name: sorted(db.execute('SELECT * FROM "' + name + '"').fetchall(), key=repr)
                for (name,) in tables}
        assert len(rows['execution_replies']) == 1
        assert db.execute("SELECT state,delivery_id FROM execution_replies").fetchall() == [('delivered', 'fixture-delivery')]
        return hashlib.sha256(repr(rows).encode()).hexdigest()


def main():
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise SystemExit('Run only on a disposable GitHub-hosted runner')
    os.umask(0o077)
    repo = Path(__file__).resolve().parents[2]
    evidence = repo / 'target/inbox-restart-evidence'
    evidence.mkdir(parents=True, exist_ok=True)
    report = {'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(), 'cases': []}
    root = Path(tempfile.mkdtemp(prefix='inbox-restart-'))
    compose = root / 'source/inbox/deploy'
    compose.mkdir(parents=True)
    state = root / 'state'
    state.mkdir(mode=0o700)
    # Local fixture CA only; HTTPS verification remains enabled.
    run('openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
        '-keyout', str(root/'key.pem'), '-out', str(root/'cert.pem'), '-subj', '/CN=localhost',
        '-addext', 'subjectAltName=DNS:localhost', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    os.environ['SSL_CERT_FILE'] = str(root/'cert.pem')
    os.environ['INBOX_BIND_IP'] = '127.0.0.1'
    os.environ['CLIPROXY_CREDENTIAL_FILE'] = str(root/'empty-credential')
    (root/'empty-credential').touch(mode=0o600)
    for name in ('compose.yml', 'nginx.conf'):
        shutil.copyfile(repo/'inbox/deploy'/name, compose/name)
    (root/'edge.conf').write_text('''events {}\nhttp { access_log off;
server { listen 8443 ssl; server_name localhost;
ssl_certificate /cert.pem; ssl_certificate_key /key.pem;
location / { proxy_pass http://127.0.0.1:8080; proxy_set_header Host $http_host; }
}}\n''')
    # Same Inbox Dockerfile and ingress configuration/topology as deployment.
    # Only origin, credential mount and loopback bind are fixture-specific.
    override = {'services': {
        'inbox': {'environment': {'INBOX_ORIGIN': 'https://localhost:8443'}},
        'edge': {'image': 'nginx:1.28-alpine', 'network_mode': 'host', 'volumes': [
            f'{root}/edge.conf:/etc/nginx/nginx.conf:ro', f'{root}/cert.pem:/cert.pem:ro',
            f'{root}/key.pem:/key.pem:ro']}}}
    (compose/'compose.override.yml').write_text(json.dumps(override))
    class MeasuredDeployment(restart.Deployment):
        outage_start = None
        def run(self, args):
            try:
                return super().run(args)
            except Exception:
                # Fixture commands contain only paths/IDs, never runtime credentials.
                report['failed_command'] = args
                raise
        def ordered_start(self):
            if self.outage_start is None:
                self.outage_start = time.monotonic()
            return super().ordered_start()
    d = MeasuredDeployment(compose, state, 'https://localhost:8443', timeout=20, outage_timeout=90)
    builder_name = 'inbox-restart-fixture-builder'
    try:
        run('docker', 'build', '-f', 'inbox/deploy/Dockerfile', '--target', 'build', '-t', 'inbox-restart-builder', '.', cwd=repo)
        run('docker', 'run', '--name', builder_name, '-e', 'RESTART_FIXTURE_DB=/tmp/restart-fixture.db',
            'inbox-restart-builder', 'cargo', 'test', '--manifest-path', 'inbox/Cargo.toml', '--locked',
            '--release', '-p', 'tessera-inboxd', '--test', 'restart_fixture', '--', '--ignored', '--exact', 'create_restart_fixture')
        run('docker', 'cp', builder_name+':/tmp/restart-fixture.db', str(root/'fixture.db'))
        run('docker', 'build', '-f', 'inbox/deploy/Dockerfile', '-t', 'tessera-inbox-qa:local', '.', cwd=repo)
        d.compose('pull', 'ingress', 'edge')
        d.compose('run', '--rm', 'init')
        d.compose('create', '--no-deps', 'inbox')
        d.compose('cp', str(root/'fixture.db'), 'inbox:/data/inbox.db')
        d.compose('run', '--rm', '--entrypoint', 'chown', 'init', '1000:1000', '/data/inbox.db')
        d.compose('up', '-d', '--no-deps', 'edge')
        d.bounded_start()
        baseline = fingerprint(root/'fixture.db')
        report['images'] = d.images()
        original = d.nginx.read_bytes()
        for name in ('same-image', 'explicit-same-image', 'config-only', 'broken-ingress-rollback'):
            config = None
            image = report['images']['inbox'] if name == 'explicit-same-image' else None
            if name in ('config-only', 'broken-ingress-rollback'):
                config = root/(name+'.conf')
                config.write_bytes(original + b'\n# config-only acceptance\n' if name == 'config-only'
                                   else original.replace(b'proxy_pass http://127.0.0.1:24171;', b'return 503;'))
            previous_config = d.nginx.read_bytes()
            failures = []
            stop = threading.Event()
            # Negative control: actually observe failed HTTPS while Inbox is healthy.
            def observe():
                while not stop.is_set():
                    try:
                        urllib.request.urlopen(d.origin, timeout=1).close()
                    except urllib.error.HTTPError as error:
                        if error.code != 503:
                            continue
                        container = d.compose('ps', '-q', 'inbox')
                        if container and d.run(['docker', 'inspect', '--format',
                            '{{if .State.Health}}{{.State.Health.Status}}{{end}}', container]) == 'healthy':
                            failures.append(time.monotonic())
                    except (OSError, RuntimeError):
                        pass
                    stop.wait(.25)
            monitor = threading.Thread(target=observe, daemon=True)
            monitor.start()
            # From first ingress removal through readiness, including automatic rollback.
            d.outage_start = None
            start = time.monotonic()
            try:
                try:
                    receipt = d.deploy(image=image, nginx=config)
                    assert name != 'broken-ingress-rollback', 'broken ingress falsely accepted'
                except RuntimeError as error:
                    if name != 'broken-ingress-rollback' or 'previous services publicly ready' not in str(error):
                        raise
                    receipts = sorted(state.glob('rollback-*/receipt.json'), key=lambda p: p.stat().st_mtime_ns)
                    receipt = json.loads(receipts[-1].read_text())
                    assert receipt['status'] == 'rolled_back_public_ready'
                    assert failures, 'negative control never observed public failure with healthy Inbox'
                    assert d.nginx.read_bytes() == previous_config
                duration = time.monotonic()-d.outage_start
                assert duration < 120, f'{name}: two-minute bound exceeded'
                restart.public_ready(d.origin)
                assert d.images() == report['images']
                inbox = d.compose('ps', '-q', 'inbox')
                ingress = d.compose('ps', '-q', 'ingress')
                assert d.run(['docker', 'inspect', '--format', '{{.HostConfig.NetworkMode}}', ingress]) == 'container:'+inbox
                d.compose('exec', '-T', 'inbox', '/usr/local/bin/inbox-backup')
                after = root/(name+'.db')
                d.copy_backup(after)
                d.verify_restore(after)
                assert after.stat().st_uid == os.getuid(), 'backup operator ownership regression'
                assert fingerprint(after) == baseline, 'durable data/ack changed'
                assert fingerprint(Path(receipt['backup'])/'inbox.db') == baseline
                report['cases'].append({'name': name, 'status': 'PASS', 'receipt': receipt['status'],
                    'outage_upper_bound_seconds': round(duration,3),
                    'total_seconds': round(time.monotonic()-start,3),
                    'healthy_inbox_public_failure_observations': len(failures),
                    'durable_tables_and_delivered_ack': 'unchanged', 'backup_restore': 'PASS'})
                (evidence/'summary.json').write_text(json.dumps(report, indent=2)+'\n')
                print(json.dumps(report['cases'][-1]), flush=True)
            finally:
                stop.set()
                monitor.join(timeout=10)
        report['status'] = 'PASS'
    except BaseException as error:
        report['status'] = 'FAIL'
        report['error'] = type(error).__name__ + ': ' + str(error)
        raise
    finally:
        (evidence/'summary.json').write_text(json.dumps(report, indent=2)+'\n')
        # Only the disposable fixture project. Never called against a live project.
        d.compose('down', '--volumes', '--remove-orphans')
        run('docker', 'rm', '-f', builder_name, stdout=subprocess.DEVNULL)
        shutil.rmtree(root)


if __name__ == '__main__':
    main()
