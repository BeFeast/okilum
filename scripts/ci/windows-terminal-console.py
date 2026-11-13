#!/usr/bin/env python3
"""Native Windows control for «Open in terminal» without Windows Terminal (#2837).

The production spawner (crates/okilum-shell/src/project_terminal/windows_console.rs)
is compiled as is and starts the System32 PowerShell in a new console, from a
working directory that holds a decoy powershell.exe. A PowerShell profile that
is active only for this control records whether stdin/stdout are console
handles, which executable runs, and the folder at the first interactive prompt.
Two negative controls reproduce the #2858 head: std handles plus a new console
(stdin redirected), and `cmd /c start powershell.exe` from the folder (decoy).
"""
import base64
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
PROFILE = r'''
if ($env:OKILUM_TERMINAL_MARKER_START) {
    [IO.File]::WriteAllText($env:OKILUM_TERMINAL_MARKER_START, ('{0}|{1}|{2}' -f [Console]::IsInputRedirected, [Console]::IsOutputRedirected, (Get-Process -Id $PID).Path))
    function global:prompt {
        [IO.File]::WriteAllText($env:OKILUM_TERMINAL_MARKER_PROMPT, (Get-Location).ProviderPath)
        [Environment]::Exit(0)
    }
}
'''


def powershell(command):
    return subprocess.run(['powershell.exe', '-NoLogo', '-NoProfile', '-Command', command],
                          text=True, stdout=subprocess.PIPE, check=True).stdout.strip()


def encoded(folder):
    literal = str(folder).replace("'", "''")
    script = f"Set-Location -LiteralPath '{literal}' -ErrorAction Stop"
    return base64.b64encode(script.encode('utf-16-le')).decode('ascii')


def same(a, b):
    return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(b))


def run(helper, mode, folder, markers, want):
    for marker in markers.values():
        marker.unlink(missing_ok=True)
    env = dict(os.environ, OKILUM_TERMINAL_MARKER_START=str(markers['start']),
               OKILUM_TERMINAL_MARKER_PROMPT=str(markers['prompt']))
    # The selected folder is the working directory, as Okilum's could be.
    started = subprocess.run([str(helper), mode, encoded(folder)], cwd=folder, env=env,
                             text=True, stdout=subprocess.PIPE, check=True).stdout
    pid = int(started.split('PID ')[1].split()[0])
    deadline = time.monotonic() + 120
    try:
        while not all(markers[name].exists() for name in want):
            if time.monotonic() > deadline:
                raise RuntimeError(f'{mode}: no {want} marker within 120 s')
            time.sleep(0.5)
        time.sleep(0.5)
        return {name: markers[name].read_text(encoding='utf-8') for name in want}
    finally:
        subprocess.run(['taskkill', '/F', '/T', '/PID', str(pid)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def main():
    system = Path(powershell('[Environment]::SystemDirectory'))
    real = system / 'WindowsPowerShell' / 'v1.0' / 'powershell.exe'
    profile = Path(powershell('$PROFILE.CurrentUserAllHosts'))
    policy = powershell('Get-ExecutionPolicy -Scope CurrentUser')
    previous = profile.read_bytes() if profile.exists() else None
    with tempfile.TemporaryDirectory(prefix='okilum-terminal-console-') as temporary:
        temporary = Path(temporary)
        helper = temporary / 'terminal-console.exe'
        decoy = temporary / 'decoy.exe'
        for source, output in (('windows-terminal-console-helper.rs', helper),
                               ('windows-terminal-decoy.rs', decoy)):
            subprocess.run(['rustc', '--edition=2021', f'scripts/ci/{source}', '-o', str(output)],
                           cwd=ROOT, check=True)
        folder = temporary / 'vault' / "-rf Папка @x 'q'"
        folder.mkdir(parents=True)
        (folder / 'powershell.exe').write_bytes(decoy.read_bytes())
        markers = {'start': temporary / 'start.txt', 'prompt': temporary / 'prompt.txt'}
        try:
            profile.parent.mkdir(parents=True, exist_ok=True)
            profile.write_text(PROFILE, encoding='utf-8-sig')
            if policy in ('Restricted', 'AllSigned', 'Undefined'):
                powershell('Set-ExecutionPolicy -Scope CurrentUser RemoteSigned -Force')

            fixed = run(helper, 'console', folder, markers, ['start', 'prompt'])
            print('console:', fixed, flush=True)
            stdin, stdout, image = fixed['start'].split('|', 2)
            if (stdin, stdout) != ('False', 'False'):
                raise RuntimeError(f'Console PowerShell has redirected std handles: {fixed}')
            if not same(image, real):
                raise RuntimeError(f'Console route ran {image}, not {real}')
            if not same(fixed['prompt'], folder):
                raise RuntimeError(f'Interactive prompt is in {fixed["prompt"]}, not {folder}')

            redirected = run(helper, 'redirected', folder, markers, ['start'])
            print('redirected (#2858 head):', redirected, flush=True)
            if not redirected['start'].startswith('True|'):
                raise RuntimeError('Negative control: std handles with a new console were not seen as redirected')

            bare = run(helper, 'bare', folder, markers, ['start'])
            print('cmd start from the folder (#2858 head):', bare, flush=True)
            if not bare['start'].startswith('DECOY|') or not same(bare['start'][6:], folder / 'powershell.exe'):
                raise RuntimeError('Negative control: cwd-first resolution did not pick the decoy')
        finally:
            if previous is None:
                profile.unlink(missing_ok=True)
            else:
                profile.write_bytes(previous)
            if policy in ('Restricted', 'AllSigned', 'Undefined'):
                powershell(f'Set-ExecutionPolicy -Scope CurrentUser {policy} -Force')
    print('Open in terminal console fallback: interactive System32 PowerShell PASS; '
          '#2858 handles and cwd decoy FAIL as required', flush=True)


if __name__ == '__main__':
    main()
