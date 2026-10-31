#!/usr/bin/env python3
"""Bounded native-client contract proof against an existing isolated T3 candidate.

Never starts, stops or updates a server. Child output and credentials stay private.
"""
import argparse
import ipaddress
import hashlib
import re
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent
FLAGS = ('pair', 'subscribe_thread', 'resume_thread', 'subscribe_shell', 'dispatch_command')
MAX_OUTPUT = 1024 * 1024


class ProbeFailure(Exception):
    """Fixed stage/code only: never stringify an untrusted peer exception."""
    def __init__(self, stage, code):
        self.stage, self.code = stage, code
        super().__init__(code)


def origin(value):
    """Match Environment's HTTPS or explicit private/loopback HTTP boundary."""
    if not isinstance(value, str) or any(c.isspace() or ord(c) < 32 for c in value) or '\\' in value:
        raise ValueError('invalid origin')
    url = urllib.parse.urlsplit(value)
    if url.scheme not in ('http', 'https') or not url.hostname or url.username is not None or url.password is not None:
        raise ValueError('invalid origin')
    if url.path not in ('', '/') or '?' in value or '#' in value:
        raise ValueError('invalid origin')
    _ = url.port  # Reject malformed/out-of-range ports before network access.
    if url.scheme == 'http' and url.hostname != 'localhost':
        try:
            address = ipaddress.ip_address(url.hostname)
        except ValueError:
            raise ValueError('insecure public origin') from None
        if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
            address = address.ipv4_mapped
        allowed = address.is_loopback or address.is_link_local
        if isinstance(address, ipaddress.IPv4Address):
            allowed |= any(address in ipaddress.ip_network(net) for net in
                           ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '100.64.0.0/10'))
        else:
            allowed |= address in ipaddress.ip_network('fc00::/7')
        if not allowed:
            raise ValueError('insecure public origin')
    return value.rstrip('/')


def private_env(home, base):
    return {'PATH': os.environ.get('PATH', os.defpath), 'LANG': 'C.UTF-8', 'TZ': 'UTC',
            'HOME': str(home), 'T3CODE_HOME': str(base),
            'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_CACHE_HOME': str(home / '.cache')}


def run_private(argv, *, env, timeout, input_data=None):
    """Use private files, a process group deadline, and reap before returning.

    A terminated leader may leave children, so always signal the whole group,
    even on normal exit. No peer stdout/stderr is propagated in failures.
    """
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(argv, env=env,
                                   stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                                   stdout=out, stderr=err,
                                   start_new_session=True)
        primary = None
        try:
            try:
                process.communicate(input=input_data, timeout=timeout)
                code = process.returncode
            except subprocess.TimeoutExpired:
                raise ProbeFailure('subprocess', 'timeout') from None
            if code != 0:
                raise ProbeFailure('subprocess', 'nonzero_exit')
            out.seek(0)
            data = out.read(MAX_OUTPUT + 1)
            if len(data) > MAX_OUTPUT:
                raise ProbeFailure('subprocess', 'oversized_output')
            return data
        except BaseException as error:
            primary = error
            raise
        finally:
            cleanup_failed = False
            try:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                except Exception:
                    cleanup_failed = True
                try:
                    process.wait(timeout=0.5)
                except subprocess.TimeoutExpired:
                    pass  # Escalate to SIGKILL below; not itself a cleanup failure.
                except Exception:
                    cleanup_failed = True
                # Kill group even when its leader has already exited.
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except Exception:
                    cleanup_failed = True
                try:
                    process.wait(timeout=5)
                except Exception:
                    cleanup_failed = True
            finally:
                # An uninterruptible leader can outlive the reap deadline. Closing
                # our credential pipe must not depend on successful reaping.
                if process.stdin is not None:
                    try:
                        process.stdin.close()
                    except Exception:
                        cleanup_failed = True
            # Preserve the original bounded timeout/nonzero/oversize diagnosis.
            # An otherwise successful child must not hide a failed cleanup either.
            if cleanup_failed and primary is None:
                raise ProbeFailure('subprocess', 'cleanup_failed') from None


def atomic_receipt(path, receipt):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(prefix='.contract-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as output:
            os.fchmod(output.fileno(), 0o600)
            json.dump(receipt, output, sort_keys=True, indent=2)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def worker(kind, configuration):
    """Hidden bounded workers keep DNS and fixture setup under a hard deadline."""
    if kind == 'descriptor':
        # Redirects cannot escape the validated candidate origin.
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                return None
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(configuration['origin'] + '/.well-known/t3/environment', timeout=10) as response:
            data = response.read(65537)
        if len(data) > 65536:
            raise ValueError('oversized descriptor')
        descriptor = json.loads(data)
        # Only the capability required for safe metadata mutation crosses out.
        version = descriptor.get('serverVersion')
        if not isinstance(version, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.+_-]{0,127}', version):
            raise ValueError('invalid server version')
        return {'protocol': descriptor.get('orchestrationProtocolVersion'), 'version': version}
    if kind == 'fixture':
        from nightly import fixture_thread
        identifier = fixture_thread(Path(configuration['binary']), Path(configuration['base']),
                                    configuration['env'], configuration['origin'],
                                    expected_title=configuration['title'])
        if not isinstance(identifier, str) or not identifier or len(identifier) > 512 or any(ord(c) < 32 for c in identifier):
            raise ValueError('invalid scratch thread')
        return {'thread_id': identifier}
    raise ValueError('invalid worker')


def checked_json(data):
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError('invalid object')
    return value


def digest(path):
    checksum = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()


def execute(args):
    receipt = {'status': 'FAIL', **{flag: False for flag in FLAGS}}
    stage = 'preflight'
    try:
        url = origin(args.url)
        binary, base, client = (Path(p).resolve(strict=True) for p in
                                (args.server_binary, args.server_base_dir, args.client_probe))
        if not base.is_dir() or not binary.is_file() or not client.is_file():
            raise ValueError('invalid candidate paths')
        # The caller supplies isolated candidate state. Do not accept a symlink
        # accidentally resolving to a different server's operational state.
        if Path(args.server_base_dir).is_symlink():
            raise ValueError('symlink candidate state')
        stage = 'schema'
        from compatibility import compare, load_baseline
        candidate_bytes = Path(args.candidate_schema).read_bytes()
        candidate = checked_json(candidate_bytes)
        baseline = load_baseline(None)
        receipt.update({'client_binary_sha256': digest(client),
                        'server_binary_sha256': digest(binary),
                        'candidate_schema_sha256': hashlib.sha256(candidate_bytes).hexdigest(),
                        'baseline_sha256': hashlib.sha256(json.dumps(baseline, sort_keys=True,
                            separators=(',', ':')).encode()).hexdigest()})
        comparison = compare(candidate, baseline)
        receipt['schema'] = 'PASS' if comparison.get('status') == 'PASS' else 'FAIL'
        receipt['schema_informational_count'] = len(comparison.get('informational', []))
        receipt['schema_breaking_count'] = len(comparison.get('breaking', []))
        receipt['schema_error_count'] = len(comparison.get('errors', []))
        if comparison.get('status') != 'PASS':
            raise ProbeFailure(stage, 'incompatible_schema')
        with tempfile.TemporaryDirectory(prefix='okilum-contract-private-') as temporary:
            home = Path(temporary)
            env = private_env(home, base)
            def internal(kind, configuration, timeout):
                return checked_json(run_private([sys.executable, str(Path(__file__).resolve()),
                                                  '--internal-worker', kind], env=env, timeout=timeout,
                                                 input_data=json.dumps(configuration).encode()))
            stage = 'descriptor'
            descriptor = internal('descriptor', {'origin': url}, 15)
            if type(descriptor.get('protocol')) is not int or descriptor['protocol'] != 2:
                raise ProbeFailure(stage, 'protocol_v2_required')
            receipt['protocol'] = 2
            receipt['serverVersion'] = descriptor['version']
            stage = 'scratch_fixture'
            title = 'Okilum contract scratch ' + str(uuid.uuid4())
            fixture = internal('fixture', {'binary': str(binary), 'base': str(base), 'env': env,
                                           'origin': url, 'title': title}, 90)
            stage = 'pairing_credential'
            pairing = checked_json(run_private([str(binary), 'auth', 'pairing', 'create', '--base-dir',
                                                 str(base), '--json'], env=env, timeout=30))
            credential = pairing.get('credential')
            if not isinstance(credential, str) or not credential.strip():
                raise ValueError('missing pairing credential')
            stage = 'native_client'
            native = checked_json(run_private([str(client)], env=env, timeout=150,
                                  input_data=json.dumps({'origin': url, 'credential': credential,
                                                         'thread_id': fixture['thread_id'],
                                                         'expected_title': title}).encode()))
            for flag in FLAGS:
                receipt[flag] = native.get(flag) is True
            if not all(receipt[flag] for flag in FLAGS):
                raise ProbeFailure(stage, 'incomplete_native_proof')
        if digest(client) != receipt['client_binary_sha256'] or digest(binary) != receipt['server_binary_sha256']:
            raise ProbeFailure('preflight', 'binary_changed')
        receipt['status'] = 'PASS'
    except BaseException as error:
        # No arbitrary exception, child output, URL, token, path or log leaks.
        receipt['stage'] = stage
        receipt['code'] = error.code if isinstance(error, ProbeFailure) else 'failed'
    atomic_receipt(args.receipt, receipt)
    return receipt


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--internal-worker':
        try:
            configuration = checked_json(sys.stdin.buffer.read(MAX_OUTPUT + 1))
            print(json.dumps(worker(sys.argv[2], configuration)))
            return 0
        except BaseException:
            return 1
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ('url', 'server-binary', 'server-base-dir', 'candidate-schema', 'client-probe', 'receipt'):
        parser.add_argument('--' + flag, required=True)
    args = parser.parse_args()
    receipt = execute(args)
    print(json.dumps(receipt, sort_keys=True))
    return 0 if receipt['status'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
