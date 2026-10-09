"""Ordered deployment and public-readiness negative controls; no live Docker."""
import json
from pathlib import Path
import tempfile
import tarfile
import unittest
from unittest.mock import patch
import restart


class Response:
    def __init__(self, status=200, body=b'{}'):
        self.status, self.body = status, body
    def __enter__(self):
        return self
    def __exit__(self, *args):
        pass
    def read(self, limit):
        return self.body[:limit]


class Opener:
    def __init__(self, responses):
        self.responses, self.requests = iter(responses), []
    def open(self, request, timeout):
        self.requests.append(request)
        return next(self.responses)


class ReadinessTests(unittest.TestCase):
    def test_public_root_and_actual_login_challenge(self):
        opener = Opener([Response(), Response(body=b'{"publicKey":{"challenge":"test"}}')])
        restart.public_ready('https://example.test', opener)
        self.assertEqual([r.full_url for r in opener.requests],
                         ['https://example.test/', 'https://example.test/api/v1/auth/login/start'])
        self.assertEqual(opener.requests[1].get_method(), 'POST')
        self.assertEqual(opener.requests[1].get_header('Origin'), 'https://example.test')
        self.assertIsNone(opener.requests[1].get_header('Cookie'))

    def test_green_local_health_does_not_mask_broken_public_ingress(self):
        with self.assertRaises(RuntimeError):
            restart.public_ready('https://example.test', Opener([Response(503)]))

    def test_root_200_is_not_enough(self):
        with self.assertRaises(RuntimeError):
            restart.public_ready('https://example.test', Opener([Response(), Response()]))

    def test_redirect_is_not_readiness(self):
        self.assertIsNone(restart.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://other.test'))


class FakeDeployment(restart.Deployment):
    def __init__(self, root):
        directory = root / 'source/inbox/deploy'
        directory.mkdir(parents=True)
        state = root / 'state'
        state.mkdir()
        super().__init__(directory, state, 'https://example.test', timeout=0,
            invocation=restart.Invocation(('docker', 'compose', '-f', str(directory/'compose.yml')),
                directory, (directory/'compose.yml',), (), None))
        self.nginx.write_text('old nginx')
        self.events = []
        self.backup_fails = False
        self.health = 'healthy'

    def preflight(self):
        pass

    def compose(self, *args):
        self.events.append(('compose', args))
        if args[:2] == ('ps', '-q'):
            return args[2] + '-container'
        return ''

    def run(self, args):
        self.events.append(('docker', tuple(args)))
        if '{{.Image}}' in args:
            return 'old-' + args[-1]
        if 'image' in args and 'inspect' in args:
            return 'new-image-id'
        return self.health

    def snapshot(self, directory, images):
        self.events.append(('backup',))
        if self.backup_fails:
            raise RuntimeError('backup refused')
        (directory / 'nginx.conf').write_bytes(self.nginx.read_bytes())


class RuntimePreflightTests(unittest.TestCase):
    def test_external_env_omission_fails_without_leaking_values(self):
        desired = {'command': ['serve', '--ai-endpoint', ''], 'environment': {'AI_ENDPOINT': ''}}
        actual = {'Cmd': ['serve', '--ai-endpoint', 'private-endpoint'], 'Entrypoint': None,
                  'Env': ['AI_ENDPOINT=private-endpoint']}
        with self.assertRaisesRegex(RuntimeError, 'resolved command differs') as error:
            restart.Deployment.validate_runtime('inbox', desired, {}, actual)
        self.assertNotIn('private-endpoint', str(error.exception))

    def test_environment_only_drift_fails(self):
        with self.assertRaisesRegex(RuntimeError, 'resolved environment differs'):
            restart.Deployment.validate_runtime('inbox', {'environment': {'AI_MODEL': ''}},
                {'Env': ['PATH=/bin']}, {'Env': ['PATH=/bin', 'AI_MODEL=private-model']})

    def test_matching_external_env_and_image_defaults_pass(self):
        restart.Deployment.validate_runtime('inbox', {'environment': {'AI_MODEL': 'fixture'}},
            {'Cmd': ['serve'], 'Entrypoint': ['entry'], 'Env': ['PATH=/bin']},
            {'Cmd': ['serve'], 'Entrypoint': ['entry'], 'Env': ['PATH=/bin', 'AI_MODEL=fixture']})


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.deploy = FakeDeployment(Path(self.temp.name))

    def mutations(self):
        return [args for kind, *rest in self.deploy.events if kind == 'compose'
                for args in rest if args[0] in ('rm', 'up')]

    def test_restart_starts_ingress_only_after_healthy_new_inbox(self):
        with patch.object(restart, 'public_ready') as public:
            result = self.deploy.deploy()
        self.assertEqual(result['status'], 'deployed_public_ready')
        public.assert_called_once_with('https://example.test')
        commands = self.mutations()
        self.assertEqual([x[-1] for x in commands], ['ingress', 'inbox', 'ingress'])
        self.assertEqual(commands[0][:3], ('rm', '--stop', '--force'))
        self.assertIn('--force-recreate', commands[1])
        self.assertIn('--no-deps', commands[2])
        events = self.deploy.events
        health = next(i for i, e in enumerate(events) if e[0] == 'docker' and '.State.Health' in str(e))
        ingress = next(i for i, e in enumerate(events) if e[0] == 'compose' and e[1][0] == 'up' and e[1][-1] == 'ingress')
        self.assertLess(health, ingress)
        self.assertLess(events.index(('backup',)), next(i for i, e in enumerate(events) if e[0] == 'compose' and e[1][0] == 'rm'))

    def test_failed_backup_never_mutates_services_or_configuration(self):
        self.deploy.backup_fails = True
        with self.assertRaisesRegex(RuntimeError, 'backup refused'):
            self.deploy.deploy('candidate')
        self.assertEqual(self.mutations(), [])
        self.assertFalse(self.deploy.active.exists())
        self.assertEqual(self.deploy.nginx.read_text(), 'old nginx')

    def test_image_and_config_deploy_roll_back_on_public_failure(self):
        candidate = Path(self.temp.name) / 'candidate.conf'
        candidate.write_text('broken nginx')
        with patch.object(restart, 'public_ready', side_effect=[RuntimeError('503'), None]) as public:
            with self.assertRaisesRegex(RuntimeError, 'previous services publicly ready'):
                self.deploy.deploy('candidate', candidate)
        self.assertEqual(public.call_count, 2)
        self.assertEqual(self.deploy.nginx.read_text(), 'old nginx')
        self.assertEqual(json.loads(self.deploy.active.read_text())['services']['inbox']['image'], 'old-inbox-container')
        self.assertEqual([c[-1] for c in self.mutations()], ['ingress', 'inbox', 'ingress'] * 2)
        receipt = next(self.deploy.state.glob('rollback-*/receipt.json'))
        self.assertEqual(json.loads(receipt.read_text())['status'], 'rolled_back_public_ready')

    def test_same_image_config_only_deploy_uses_same_order(self):
        candidate = Path(self.temp.name) / 'candidate.conf'
        candidate.write_text('new valid nginx')
        with patch.object(restart, 'public_ready'):
            self.deploy.deploy(nginx=candidate)
        self.assertEqual(self.deploy.nginx.read_text(), 'new valid nginx')
        self.assertEqual([c[-1] for c in self.mutations()], ['ingress', 'inbox', 'ingress'])

    def test_unhealthy_inbox_never_starts_ingress(self):
        self.deploy.health = 'unhealthy'
        with patch.object(restart, 'public_ready') as public:
            with self.assertRaisesRegex(RuntimeError, 'Rollback failed'):
                self.deploy.deploy()
        public.assert_not_called()
        self.assertFalse(any(c[0] == 'up' and c[-1] == 'ingress' for c in self.mutations()))

    def test_source_snapshot_excludes_runtime_data_and_credentials(self):
        root = self.deploy.compose_dir.parent.parent
        (root / 'code.py').write_text('reviewed source')
        for name in ('data', 'backups', 'fixture-vault', 'secrets'):
            folder = self.deploy.compose_dir / name
            folder.mkdir()
            (folder / 'must-not-copy').write_text('private runtime bytes')
        (self.deploy.compose_dir / '.env').write_text('private environment')
        directory = self.deploy.state / 'snapshot-test'
        directory.mkdir()
        with patch.object(self.deploy, "verify_restore"), patch.object(self.deploy, "copy_backup"):
            restart.Deployment.snapshot(self.deploy, directory, {'inbox': 'old-inbox', 'ingress': 'old-ingress'})
        with tarfile.open(directory / 'source.tar.gz') as archive:
            names = archive.getnames()
        self.assertIn('source/code.py', names)
        self.assertIn('source/inbox/deploy/nginx.conf', names)
        self.assertFalse(any('must-not-copy' in name or name.endswith('/.env') for name in names))
        self.assertTrue(any(e[0] == 'compose' and e[1][:3] == ('exec', '-T', 'inbox') for e in self.deploy.events))

    def test_backup_export_is_owned_by_operator_and_private(self):
        import os
        destination = Path(self.temp.name) / 'export.db'
        def export(command, **kwargs):
            self.assertEqual(command[-5:], ['exec', '-T', 'inbox', 'cat', '/backups/inbox-latest.db'])
            kwargs['stdout'].write(b'snapshot-bytes')
            return type('Result', (), {'returncode': 0})()
        before = os.umask(0o077)
        try:
            with patch.object(restart.subprocess, 'run', side_effect=export):
                self.deploy.copy_backup(destination)
        finally:
            os.umask(before)
        self.assertEqual(destination.read_bytes(), b'snapshot-bytes')
        self.assertEqual(destination.stat().st_uid, os.getuid())
        self.assertEqual(destination.stat().st_mode & 0o777, 0o600)

    def test_deadline_interrupts_whole_ordered_start(self):
        import signal
        import time
        self.deploy.outage_timeout = .01
        previous = signal.getsignal(signal.SIGALRM)
        with patch.object(self.deploy, 'ordered_start', side_effect=lambda: time.sleep(.2)):
            with self.assertRaisesRegex(restart.OutageTimeout, 'Outage budget'):
                self.deploy.bounded_start()
        self.assertEqual(signal.getsignal(signal.SIGALRM), previous)
        self.assertEqual(signal.getitimer(signal.ITIMER_REAL), (0.0, 0.0))

    def test_public_retry_loop_does_not_swallow_global_deadline(self):
        import time
        self.deploy.timeout = 120
        self.deploy.outage_timeout = .01
        with patch.object(restart, 'public_ready', side_effect=lambda origin: time.sleep(.2)):
            with self.assertRaises(restart.OutageTimeout):
                self.deploy.bounded_start()

    def test_total_outage_timeout_triggers_rollback(self):
        with patch.object(self.deploy, 'ordered_start', side_effect=[TimeoutError('deadline'), None]):
            with self.assertRaisesRegex(RuntimeError, 'previous services publicly ready'):
                self.deploy.deploy()
        receipt = next(self.deploy.state.glob('rollback-*/receipt.json'))
        self.assertEqual(json.loads(receipt.read_text())['status'], 'rolled_back_public_ready')

    def test_restore_check_rejects_missing_tables(self):
        import sqlite3
        snapshot = Path(self.temp.name) / 'inbox.db'
        with restart.closing(sqlite3.connect(snapshot)) as db:
            db.execute('CREATE TABLE captures (id INTEGER)')
        with self.assertRaises(sqlite3.OperationalError):
            restart.Deployment.verify_restore(snapshot)
        self.assertFalse(list(snapshot.parent.glob('restore-check-*')))

    def test_broken_rollback_is_never_reported_as_ready(self):
        with patch.object(restart, 'public_ready', side_effect=RuntimeError('503')):
            with self.assertRaisesRegex(RuntimeError, 'Rollback failed'):
                self.deploy.deploy()
        receipt = next(self.deploy.state.glob('rollback-*/receipt.json'))
        self.assertEqual(json.loads(receipt.read_text())['status'], 'rollback_failed')



class TopologyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.d = FakeDeployment(Path(self.temp.name))
        self.resolved = {'services': {'inbox': {
            'volumes': [{'type': 'bind', 'source': '/private/credential',
                         'target': '/credential', 'read_only': True}],
            'ports': [{'target': 8080, 'published': '8080', 'host_ip': '127.0.0.1'}],
            'networks': {'default': {}}, 'tmpfs': ['/tmp'], 'read_only': True}},
            'networks': {'default': {'name': 'fixture_default'}}}
        self.actual = {'Mounts': [{'Type': 'bind', 'Source': '/private/credential',
                                  'Destination': '/credential', 'RW': False}],
            'HostConfig': {'ReadonlyRootfs': True, 'Tmpfs': {'/tmp': ''},
                           'PortBindings': {'8080/tcp': [{'HostIp': '127.0.0.1', 'HostPort': '8080'}]}},
            'NetworkSettings': {'Networks': {'fixture_default': {}}}}

    def validate(self):
        self.d.validate_topology('inbox', self.resolved, self.actual)

    def test_matching_topology_positive_control(self):
        self.validate()

    def test_changed_credential_source_refused(self):
        self.resolved['services']['inbox']['volumes'][0]['source'] = '/different/credential'
        with self.assertRaisesRegex(RuntimeError, 'mounts differ') as error:
            self.validate()
        self.assertNotIn('/different', str(error.exception))

    def test_mount_target_and_read_only_refused(self):
        for key, value in [('target', '/other'), ('read_only', False)]:
            with self.subTest(key=key):
                mount = self.resolved['services']['inbox']['volumes'][0]
                old = mount[key]
                mount[key] = value
                with self.assertRaisesRegex(RuntimeError, 'mounts differ'):
                    self.validate()
                mount[key] = old

    def test_changed_bind_ip_refused(self):
        self.resolved['services']['inbox']['ports'][0]['host_ip'] = '0.0.0.0'
        with self.assertRaisesRegex(RuntimeError, 'port bindings differ'):
            self.validate()

    def test_changed_network_refused(self):
        self.actual['NetworkSettings']['Networks'] = {'another_default': {}}
        with self.assertRaisesRegex(RuntimeError, 'networks differ'):
            self.validate()

    def test_wrong_shared_namespace_refused(self):
        self.resolved['services']['inbox']['network_mode'] = 'service:owner'
        self.actual['HostConfig']['NetworkMode'] = 'container:owner-container'
        self.validate()
        self.actual['HostConfig']['NetworkMode'] = 'container:obsolete'
        with self.assertRaisesRegex(RuntimeError, 'namespace differs'):
            self.validate()

    def test_named_volume_identity(self):
        self.resolved['services']['inbox']['volumes'] = [
            {'type': 'volume', 'source': 'data', 'target': '/data'}]
        self.resolved['volumes'] = {'data': {'name': 'fixture_data'}}
        self.actual['Mounts'] = [{'Type': 'volume', 'Name': 'fixture_data',
                                 'Destination': '/data', 'RW': True}]
        self.validate()
        self.actual['Mounts'][0]['Name'] = 'other_data'
        with self.assertRaisesRegex(RuntimeError, 'mounts differ'):
            self.validate()


class InvocationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.scripts = [self.root/name for name in ('activate.sh', 'rollback.sh')]
        self.command = f'docker compose --env-file {self.root}/runtime.env -p fixture -f {self.root}/compose.yml'
        for script in self.scripts:
            script.write_text(f'#!/bin/sh\nset -eu\ncd {self.root}\n{self.command} up -d\n')

    def test_scripts_drive_forward_and_rollback(self):
        invocation = restart.Invocation.from_scripts(*self.scripts)
        self.assertEqual(invocation.env_files, (self.root/'runtime.env',))
        active = self.root/'active.json'
        active.touch()
        self.assertEqual(invocation.command(active, 'up', '-d'),
                         self.command.split()+['-f', str(active), 'up', '-d'])

    def test_different_script_invocations_refused(self):
        self.scripts[1].write_text(self.scripts[1].read_text().replace('runtime.env', 'other.env'))
        with self.assertRaisesRegex(ValueError, 'invocations differ'):
            restart.Invocation.from_scripts(*self.scripts)

    def test_dynamic_or_exported_environment_refused(self):
        for command in ('export INBOX_BIND_IP=127.0.0.1\n'+self.command,
                        'INBOX_BIND_IP=127.0.0.1 '+self.command,
                        self.command.replace('runtime.env', '$ENV_FILE')):
            with self.subTest(command=command):
                self.scripts[0].write_text(f'cd {self.root}\n{command} up -d\n')
                with self.assertRaises(ValueError):
                    restart.Invocation.read(self.scripts[0])

    def test_labels_positive_control_and_env_provenance(self):
        invocation = restart.Invocation.from_scripts(*self.scripts)
        labels = {'com.docker.compose.project.config_files': str(self.root/'compose.yml'),
                  'com.docker.compose.project.working_dir': str(self.root),
                  'com.docker.compose.project': 'fixture',
                  'com.docker.compose.project.environment_file': str(self.root/'runtime.env')}
        invocation.verify_labels(labels, self.root/'absent.json', 'fixture')
        labels.pop('com.docker.compose.project.environment_file')
        with self.assertRaisesRegex(RuntimeError, 'env-file provenance'):
            invocation.verify_labels(labels, self.root/'absent.json', 'fixture')

if __name__ == '__main__':
    unittest.main()
