#!/usr/bin/env python3
"""Bounded Kibana compatibility run for the Remote Log view connector (#1440).

On a disposable GitHub-hosted runner only: start one digest-pinned
Elasticsearch + Kibana pair in Docker, mint an ephemeral read-only API key
inside it, run the debug-only `kibana_compat` example (the product transport)
against it, write a per-version report and remove the containers.

Nothing here can reach a real deployment. Endpoints are loopback ports this
script publishes; credentials are random per run and passed to Docker and the
runner through the environment, never argv or logs. Synthetic data only.

Bounds: Elasticsearch 1.5 GiB (512 MiB heap), Kibana 2 GiB (1 GiB Node heap),
2 CPUs each; 3 min for Elasticsearch, 5 min for Kibana, 5 min for the runner
(which bounds itself to 4 min, and its live phase to 60 s); 600 documents.
"""
import argparse
import base64
import json
import os
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ES_REPO = 'docker.elastic.co/elasticsearch/elasticsearch'
KIBANA_REPO = 'docker.elastic.co/kibana/kibana'
# version -> (Elasticsearch index digest, Kibana index digest). Read from
# docker.elastic.co on 2026-11-17. 9.5.4 is the version observed on the
# Integration deployment (docs/remote-log-compatibility.md); 9.5.5 and 8.19.25
# are the newest 9.x and 8.x; 8.17.10 is the oldest maintained 8.x line.
IMAGES = {
    '8.17.10': ('sha256:cff047b5ac7bc2c8a470b49c5733e64c1689024455e85630288ef5d9a810f6ca',
                'sha256:a4a7d98dad6145cff03d1bcff38462876348dbd64eca12ded61b94dbb1b5398e'),
    '8.19.25': ('sha256:5bfdf4313b9bc05c0d46fe219d6e67710eb0c7654ed6c275167bdf9beefc56f0',
                'sha256:c6750ac07387a705f0bcb5b351ef2192cf8a66c16ad0080f701dc4181a91480c'),
    '9.5.4': ('sha256:82ac14f43fe701992e601f4cc81e1c0d7dbc5a2576d8cd736006452925df4026',
              'sha256:b1b4cea273350ffcb67292d7b90dd034598c1627bfa3cd0783315e9237dbe737'),
    '9.5.5': ('sha256:13b4a40bb095774f5fda635e45fd5677cc6a26565e42b24997b0c72b88e75984',
              'sha256:13b72ce7222c36e4213229004ae62a87aeb32fd372ebfa6fb38f7a3285fdbb78'),
}
ES_PORT = 9200
KIBANA_PORT = 5601
ES_URL = f'http://127.0.0.1:{ES_PORT}'
KIBANA_URL = f'http://127.0.0.1:{KIBANA_PORT}'
DATA_VIEW = 'okilum-compat'
PATTERNS = ['okilum-compat-*', 'okilum-conflict-*']
ES_LIMITS = ['--memory', '1536m', '--memory-swap', '1536m', '--cpus', '2']
KIBANA_LIMITS = ['--memory', '2g', '--memory-swap', '2g', '--cpus', '2']
ES_START_SECONDS = 180
KIBANA_START_SECONDS = 300
RUNNER_SECONDS = 300


def images(version):
    es, kibana = IMAGES[version]
    return f'{ES_REPO}:{version}@{es}', f'{KIBANA_REPO}:{version}@{kibana}'


def require_hosted(env):
    """Containers run only on GitHub's disposable hosted runners."""
    if env.get('GITHUB_ACTIONS') != 'true' or env.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise SystemExit('kibana-compat runs only on GitHub-hosted runners')


def api_key_request():
    """Read-only on the synthetic indices; Discover read for data-view lookups.

    Kibana 9 renamed the Discover feature to discover_v2; the legacy name is
    kept so the same key works on 8.x.
    """
    return {
        'name': 'okilum-compat-reader',
        'expiration': '1h',
        'role_descriptors': {
            'okilum_compat_reader': {
                'cluster': [],
                'indices': [{'names': PATTERNS, 'privileges': ['read', 'view_index_metadata']}],
                'applications': [{
                    'application': 'kibana-.kibana',
                    'privileges': ['feature_discover.read', 'feature_discover_v2.read'],
                    'resources': ['*'],
                }],
            },
        },
    }


def kibana_available(body):
    try:
        return json.loads(body)['status']['overall']['level'] == 'available'
    except (ValueError, KeyError, TypeError):
        return False


def summary(version, report):
    """Markdown table for the job summary; details never contain credentials."""
    rows = [f'### Kibana {version} (synthetic, containerised)', '',
            '| check | outcome | seconds | detail |', '|---|---|---|---|']
    for check in report.get('checks', []):
        detail = str(check.get('detail', '')).replace('|', '\\|').replace('\n', ' ')
        rows.append(f"| {check.get('name')} | {check.get('outcome')} | {check.get('seconds')} | {detail} |")
    return '\n'.join(rows) + '\n'


def http(method, url, password=None, body=None, headers=None, timeout=10):
    request = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode())
    if body is not None:
        request.add_header('Content-Type', 'application/json')
    if password is not None:
        token = base64.b64encode(f'elastic:{password}'.encode()).decode()
        request.add_header('Authorization', f'Basic {token}')
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode(errors='replace')
    except (urllib.error.URLError, OSError):
        return None, ''


def expect(method, url, password, body=None, headers=None):
    status, text = http(method, url, password, body, headers)
    if status is None or not 200 <= status < 300:
        raise SystemExit(f'{method} {url.split("?")[0]} failed: HTTP {status}')
    return json.loads(text) if text else {}


def wait(what, seconds, ready):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if ready():
            return
        time.sleep(3)
    raise SystemExit(f'{what} was not ready within {seconds} s')


def docker(*args, env=None, timeout=600):
    subprocess.run(['docker', *args], check=True, env=env, timeout=timeout, stdout=subprocess.DEVNULL)


def run(args):
    require_hosted(os.environ)
    es_image, kibana_image = images(args.version)
    elastic = secrets.token_urlsafe(24)
    kibana_system = secrets.token_urlsafe(24)
    encryption = secrets.token_hex(32)
    for value in (elastic, kibana_system, encryption):
        print(f'::add-mask::{value}', flush=True)
    stamp = f'{os.getpid()}'
    network, es_name, kibana_name = f'okc-{stamp}', f'okc-es-{stamp}', f'okc-kb-{stamp}'
    secret_env = {**os.environ, 'ELASTIC_PASSWORD': elastic, 'ELASTICSEARCH_PASSWORD': kibana_system,
                  'XPACK_ENCRYPTEDSAVEDOBJECTS_ENCRYPTIONKEY': encryption}
    succeeded = False
    try:
        docker('network', 'create', network)
        docker('run', '-d', '--name', es_name, '--network', network, '-p', f'127.0.0.1:{ES_PORT}:9200', *ES_LIMITS,
               '-e', 'discovery.type=single-node', '-e', 'xpack.security.enabled=true',
               '-e', 'xpack.security.http.ssl.enabled=false', '-e', 'xpack.security.transport.ssl.enabled=false',
               '-e', 'xpack.ml.enabled=false', '-e', 'ingest.geoip.downloader.enabled=false',
               '-e', 'cluster.routing.allocation.disk.threshold_enabled=false',
               '-e', 'ES_JAVA_OPTS=-Xms512m -Xmx512m', '-e', 'ELASTIC_PASSWORD', es_image, env=secret_env)
        wait('Elasticsearch', ES_START_SECONDS, lambda: http(
            'GET', f'{ES_URL}/_cluster/health?wait_for_status=yellow&timeout=2s', elastic)[0] == 200)
        expect('POST', f'{ES_URL}/_security/user/kibana_system/_password', elastic, {'password': kibana_system})
        docker('run', '-d', '--name', kibana_name, '--network', network, '-p', f'127.0.0.1:{KIBANA_PORT}:5601',
               *KIBANA_LIMITS, '-e', f'ELASTICSEARCH_HOSTS=http://{es_name}:9200',
               '-e', 'ELASTICSEARCH_USERNAME=kibana_system', '-e', 'ELASTICSEARCH_PASSWORD',
               '-e', 'XPACK_ENCRYPTEDSAVEDOBJECTS_ENCRYPTIONKEY', '-e', 'NODE_OPTIONS=--max-old-space-size=1024',
               kibana_image, env=secret_env)
        wait('Kibana', KIBANA_START_SECONDS, lambda: kibana_available(
            http('GET', f'{KIBANA_URL}/api/status', elastic)[1]))
        expect('POST', f'{KIBANA_URL}/api/data_views/data_view', elastic,
               {'data_view': {'id': DATA_VIEW, 'title': PATTERNS[0], 'timeFieldName': '@timestamp',
                              'name': 'Okilum compatibility'}},
               {'kbn-xsrf': 'true'})
        key = expect('POST', f'{ES_URL}/_security/api_key', elastic, api_key_request())['encoded']
        print(f'::add-mask::{key}', flush=True)
        runner_env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': os.environ.get('HOME', '/'),
                      'OKC_KIBANA_URL': KIBANA_URL, 'OKC_ES_URL': ES_URL, 'OKC_ES_PASSWORD': elastic,
                      'OKC_API_KEY': key, 'OKC_DATA_VIEW': DATA_VIEW, 'OKC_VERSION': args.version,
                      'OKC_REPORT': str(args.report)}
        result = subprocess.run([str(args.runner)], env=runner_env, timeout=RUNNER_SECONDS)
        succeeded = result.returncode == 0
    finally:
        if not succeeded:
            for name in (es_name, kibana_name):
                subprocess.run(['docker', 'logs', '--tail', '40', name], timeout=60, check=False)
        subprocess.run(['docker', 'rm', '-f', es_name, kibana_name], timeout=120, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(['docker', 'network', 'rm', network], timeout=60, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    report = json.loads(Path(args.report).read_text()) if Path(args.report).is_file() else {}
    text = summary(args.version, report)
    print(text)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a', encoding='utf-8') as out:
            out.write(text)
    return 0 if succeeded else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--version', required=True, choices=sorted(IMAGES))
    parser.add_argument('--runner', required=True, type=Path)
    parser.add_argument('--report', required=True, type=Path)
    return run(parser.parse_args(argv))


if __name__ == '__main__':
    sys.exit(main())
