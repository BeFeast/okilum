#!/usr/bin/env python3
"""#2589: pair a new computer through a running `okilum sync serve`.

Two isolated profiles (OKILUM_STATE_DIR, HOME and XDG directories under one
temporary root), each with its own pinned Syncthing: never a system service or
the user's daemon. Profile A runs `okilum sync serve`; a second command in A,
`okilum sync invite --approve-first`, runs through it; profile B connects with
the printed URL. Both commands and both `status` reports show the pairing, and
A's serve keeps running throughout.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import xml.etree.ElementTree as ET

WAIT = 180
OFFLINE = {
    'globalAnnounceEnabled': 'false',
    'localAnnounceEnabled': 'false',
    'relaysEnabled': 'false',
    'natEnabled': 'false',
    'urAccepted': '-1',
    'autoUpgradeIntervalH': '0',
    'startBrowser': 'false',
}


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def wait_until(label, ready, process=None, timeout=60):
    deadline = time.monotonic() + timeout
    while not ready():
        if process is not None and process.poll() is not None:
            raise RuntimeError(f'{label}: process exited with {process.returncode}')
        if time.monotonic() > deadline:
            raise RuntimeError(f'{label}: timed out')
        time.sleep(0.2)


def last_json(text):
    lines = [line for line in text.splitlines() if line.startswith('{')]
    if not lines:
        raise RuntimeError(f'no JSON result in {text!r}')
    return json.loads(lines[-1])


class Profile:
    def __init__(self, root, name, okilum, syncthing):
        self.name = name
        self.root = root / name
        self.okilum = okilum
        self.syncthing = syncthing
        self.env = {
            'PATH': '/usr/bin:/bin',
            'LANG': 'C.UTF-8',
            'HOME': str(self.root / 'home'),
            'OKILUM_STATE_DIR': str(self.root / 'state'),
        }
        for kind in ('state', 'config', 'data', 'cache'):
            self.env[f'XDG_{kind.upper()}_HOME'] = str(self.root / 'xdg' / kind)
        for key, value in self.env.items():
            if key in ('HOME', 'OKILUM_STATE_DIR') or key.startswith('XDG_'):
                Path(value).mkdir(parents=True)
        # Discovered as this profile's own runtime: $XDG_STATE_HOME/syncthing.
        self.config = Path(self.env['XDG_STATE_HOME']) / 'syncthing' / 'config.xml'
        self.socket = Path(self.env['OKILUM_STATE_DIR']) / 'sync' / 'direct' / 'control.sock'
        self.processes = []

    def start_syncthing(self):
        home = self.config.parent
        fixture_env = {'HOME': self.env['HOME'], 'GOMAXPROCS': '2', 'STMONITORED': '1'}
        subprocess.run([self.syncthing, 'generate', '--home', home], env=fixture_env,
                       check=True, stdout=subprocess.DEVNULL)
        rest, listen = free_port(), free_port()
        tree = ET.parse(self.config)
        configuration = tree.getroot()
        for folder in configuration.findall('folder'):
            configuration.remove(folder)
        gui = configuration.find('gui')
        gui.find('address').text = f'127.0.0.1:{rest}'
        key = gui.find('apikey').text
        options = configuration.find('options')
        for name, value in OFFLINE.items():
            element = options.find(name)
            if element is None:
                element = ET.SubElement(options, name)
            element.text = value
        for element in options.findall('listenAddress'):
            options.remove(element)
        ET.SubElement(options, 'listenAddress').text = f'tcp://127.0.0.1:{listen}'
        tree.write(self.config)
        log = open(home / 'fixture.log', 'wb')
        daemon = self.start(
            [self.syncthing, 'serve', '--no-browser', '--no-restart', '--no-upgrade', '--home', home],
            fixture_env, log, log)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        request = urllib.request.Request(f'http://127.0.0.1:{rest}/rest/system/version',
                                         headers={'X-API-Key': key})

        def ready():
            try:
                with opener.open(request, timeout=2) as response:
                    return response.status == 200
            except OSError:
                return False
        wait_until(f'{self.name} Syncthing REST', ready, daemon)

    def start(self, command, env, stdout, stderr):
        process = subprocess.Popen(command, env=env, stdout=stdout, stderr=stderr,
                                   start_new_session=True)
        self.processes.append(process)
        return process

    def sync(self, *arguments):
        """Run one `okilum sync` command to completion."""
        result = subprocess.run([self.okilum, 'sync', *arguments], env=self.env,
                                capture_output=True, text=True, timeout=WAIT + 60)
        print(f'[{self.name}] okilum sync {arguments[0]} -> {result.returncode}\n'
              f'{result.stdout}{result.stderr}', flush=True)
        return result

    def spawn(self, label, *arguments):
        """Start `okilum sync` in the background; output goes to files."""
        stdout = self.root / f'{label}.out'
        stderr = self.root / f'{label}.err'
        process = self.start([self.okilum, 'sync', *arguments], self.env,
                             open(stdout, 'w'), open(stderr, 'w'))
        return process, stdout, stderr

    def stop(self):
        for process in reversed(self.processes):
            if process.poll() is not None:
                continue
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(10)
            except ProcessLookupError:
                pass


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def names(status):
    return [peer['name'] for peer in status['peers']]


def run(okilum, syncthing, root):
    hub = Profile(root, 'hub', okilum, syncthing)
    laptop = Profile(root, 'laptop', okilum, syncthing)
    try:
        for profile in (hub, laptop):
            profile.start_syncthing()
        for profile in (hub, laptop):
            selected = profile.sync('select', str(profile.config))
            check(selected.returncode == 0, f'{profile.name}: select failed')
        serve, serve_out, serve_err = hub.spawn('serve', 'serve')
        wait_until('hub serve control socket', hub.socket.exists, serve)
        check(hub.socket.stat().st_mode & 0o777 == 0o600, 'control socket must be 0600')

        # A second command in the hub's profile: serve holds the Sync journal,
        # so this succeeds only through the control channel.
        invite, invite_out, invite_err = hub.spawn(
            'invite', 'invite', '--approve-first', '--name', 'Hub', '--wait', str(WAIT))
        url = None

        def printed():
            nonlocal url
            lines = invite_out.read_text().splitlines()
            url = lines[0].strip() if lines else None
            return bool(url)
        wait_until('pairing URL from invite', printed, invite, timeout=WAIT)
        check(url.startswith('okilum-sync://'), f'unexpected first line {url!r}')

        connected = laptop.sync('connect', url, '--name', 'Laptop', '--wait', str(WAIT))
        check(connected.returncode == 0, 'laptop: connect failed')
        joined = last_json(connected.stdout)
        check(joined['paired'] and joined['peer']['name'] == 'Hub', f'connect result {joined}')

        invite.wait(WAIT)
        print(f'[hub] okilum sync invite -> {invite.returncode}\n'
              f'{invite_out.read_text()}{invite_err.read_text()}', flush=True)
        check(invite.returncode == 0, 'hub: invite failed')
        invited = last_json(invite_out.read_text())
        check(invited['paired'] and invited['peer']['name'] == 'Laptop', f'invite result {invited}')
        check(serve.poll() is None, 'hub serve stopped during pairing')

        hub_status = hub.sync('status')
        check(hub_status.returncode == 0, 'hub: status through serve failed')
        check(names(last_json(hub_status.stdout)) == ['Laptop'], 'hub does not list Laptop')
        laptop_status = laptop.sync('status')
        check(laptop_status.returncode == 0, 'laptop: status failed')
        check(names(last_json(laptop_status.stdout)) == ['Hub'], 'laptop does not list Hub')
        check(serve.poll() is None, 'hub serve stopped')
        print('PASS: invite --approve-first through a running okilum sync serve paired both '
              'profiles', flush=True)
        return 0
    except Exception:
        for profile in (hub, laptop):
            for log in sorted(profile.root.glob('*.out')) + sorted(profile.root.glob('*.err')):
                print(f'--- {profile.name}/{log.name}\n{log.read_text()[-4000:]}', flush=True)
        raise
    finally:
        for profile in (hub, laptop):
            profile.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--okilum', default='target/debug/okilum', type=Path)
    parser.add_argument('--syncthing', required=True, type=Path)
    arguments = parser.parse_args()
    okilum = arguments.okilum.resolve()
    with tempfile.TemporaryDirectory(prefix='okilum-sync-cli-') as temporary:
        return run(okilum, arguments.syncthing.resolve(), Path(temporary))


if __name__ == '__main__':
    sys.exit(main())
