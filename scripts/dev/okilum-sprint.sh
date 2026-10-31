#!/bin/bash
# Local-only Mac sprint build. Run in Terminal, never through the release lane.
set -Eeuo pipefail
if [[ $# != 1 ]]; then echo "Usage: $0 <branch>" >&2; exit 2; fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'Okilum Sprint requires an Apple Silicon Mac.' >&2; exit 1
fi
# Bootstrap diagnostics before fetch: this one downloaded file is sufficient.
umask 077
started=$SECONDS
stage=preflight
stage_started=$SECONDS
lock_owned=0
restore_on_failure=0
bootstrap=$(mktemp -d "${TMPDIR:-/tmp}/okilum-sprint.XXXXXX")
reporter="$bootstrap/sprint-report.py"
context="$bootstrap/sprint-context.json"
branch=$1
sha=unknown
short_sha=unknown
root="$bootstrap"
app="$HOME/Applications/Okilum Sprint.app"
log="$bootstrap/build.log"
run_log="$bootstrap/stages.log"
cat > "$reporter" <<'SPRINT_REPORTER'
#!/usr/bin/env python3
"""Bounded Sprint diagnostics; no clipboard API, no credentials, no retries."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ENDPOINT = 'https://pomoi.co/api/feedback/okilum'
MAX_LOG = 250 * 1024


def redact(text):
    # Known secret environment values are removed even without a field label.
    for key, value in os.environ.items():
        if len(value) >= 8 and re.search(r'token|secret|password|authorization|api_key', key, re.I):
            text = text.replace(value, '[redacted]')
    text = re.sub(r'(?i)(bearer\s+)[^\s"\']+', r'\1[redacted]', text)
    text = re.sub(r'(?i)((?:[\w-]*(?:token|password|secret|authorization|clipboard|api[_-]?key)[\w-]*)["\']?\s*[:=]\s*["\']?)[^\n,;"\']*', r'\1[redacted]', text)
    text = re.sub(r'\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b', '[redacted-jwt]', text)
    text = re.sub(r'(?i)(https?://)[^\s/@]+:[^\s/@]+@', r'\1[redacted]@', text)
    return text


def tail(path, lines=200, cap=40000):
    try:
        with Path(path).open('rb') as f:
            f.seek(max(0, f.seek(0, 2) - cap))
            return f.read(cap).decode('utf-8', errors='replace').splitlines()[-lines:]
    except (OSError, ValueError):
        return ['(not available)']


def run(command, timeout=8, env=None, cap=18000):
    try:
        with tempfile.TemporaryFile() as output:
            process = subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT,
                                       env=env, start_new_session=True)
            timed_out = False
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            output.seek(0, 2)
            output.seek(max(0, output.tell() - cap))
            return f'exit={process.returncode} timeout={timed_out}\n' + output.read(cap).decode('utf-8', errors='replace')
    except OSError as error:
        return f'(unavailable: {error})'


def repair_cache(root):
    root = Path(root).resolve()
    target = root / 'target'
    stamp = target / '.okilum-sprint-root'
    previous = stamp.read_text().strip() if stamp.is_file() else ''
    old = {previous, str(Path.home() / 'okilum-sprint')}
    old.discard('')
    old.discard(str(root))
    removed = []
    for build in [target / 'sprint' / 'build', *target.glob('*/sprint/build')]:
        if not build.is_dir() or build.is_symlink() or not build.resolve().is_relative_to(target.resolve()):
            continue
        for directory in build.iterdir():
            output = directory / 'output'
            if directory.is_symlink() or output.is_symlink() or not output.is_file():
                continue
            text = output.read_text(errors='replace')
            icons = re.findall(r'cargo::?rustc-env=GPUI_KIT_DEFAULT_ICONS_DIR=([^\n]+)', text)
            stale = any(path in text for path in old) or any(not path.startswith(str(root / 'src') + '/') for path in icons)
            if stale:
                # Only dedicated build-script outputs and their Cargo fingerprint.
                fingerprint = build.parent / '.fingerprint' / directory.name
                shutil.rmtree(directory)
                if fingerprint.is_dir() and not fingerprint.is_symlink():
                    shutil.rmtree(fingerprint)
                removed.append(directory.name)
    target.mkdir(parents=True, exist_ok=True)
    stamp.write_text(str(root) + '\n')
    print('Sprint cache: invalidated stale build-script outputs: ' + (', '.join(removed) or 'none'))


def collect(context, stage, app_log=None):
    app = Path(context['app'])
    sections = []
    def add(name, value):
        sections.append(f'\n=== {name} ===\n' + str(value))
    add('Sprint context', json.dumps(context, sort_keys=True))
    add('Stage log (last 200 lines)', '\n'.join(tail(context.get('stage_log', context['build_log']))))
    add('Build log (last 200 lines)', '\n'.join(tail(context['build_log'])))
    add('codesign staged bundle', run(['codesign', '--verify', '--deep', '--strict', '-v', str(Path(context['root']) / 'Okilum Sprint.app')]))
    add('codesign installed bundle', run(['codesign', '--verify', '--deep', '--strict', '-v', str(app)]))
    try:
        info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
        keys = ['CFBundleIdentifier', 'CFBundleExecutable', 'CFBundleVersion', 'CFBundleShortVersionString', 'OkilumSourceCommit', 'LSEnvironment']
        safe = {k: info.get(k) for k in keys}
        if isinstance(safe.get('LSEnvironment'), dict):
            safe['LSEnvironment'] = {k: v if k == 'OKILUM_SPRINT_KEYS' else '[redacted]' for k, v in safe['LSEnvironment'].items()}
        add('Installed Info.plist', json.dumps(safe, default=str))
    except (OSError, ValueError, plistlib.InvalidFileException) as error:
        add('Installed Info.plist', f'(unavailable: {error})')
    if stage == 'launch':
        env = dict(os.environ)
        state = Path.home() / 'Library/Application Support/com.befeast.okilum.sprint'
        env.update(OKILUM_STATE_DIR=str(state), OKILUM_SPRINT_KEYS='1',
                   XDG_CONFIG_HOME=str(state / 'config'), XDG_CACHE_HOME=str(state / 'cache'),
                   XDG_STATE_HOME=str(state / 'state'), XDG_DATA_HOME=str(state / 'data'))
        add('Direct binary execution (10 second deadline)', run([str(app / 'Contents/MacOS/okilum')], timeout=10, env=env))
    add('Unified app log (last 2 minutes)', run(['/usr/bin/log', 'show', '--style', 'compact', '--predicate', 'process == "okilum"', '--last', '2m']))
    if app_log:
        add('App diagnostics', '\n'.join(tail(app_log, 250)))
    add('Sprint key diagnostics', '\n'.join(tail(Path.home() / 'Library/Logs/okilum-sprint-keys.log', 500)))
    return redact('\n'.join(sections)).encode('utf-8')[-MAX_LOG:].decode('utf-8', errors='replace')


def send(context, stage, log, endpoint=ENDPOINT):
    title = f"[Sprint report] {context['branch']} {context['sha'][:8]} {stage} FAILED"
    meta = dict(context, os='macos', os_version=run(['sw_vers', '-productVersion']).split('\n', 1)[-1].strip(),
                arch=run(['uname', '-m']).split('\n', 1)[-1].strip(), stage=stage)
    fields = {'title': title[:240], 'text': title[:240] + '\nAutomatic Sprint failure diagnostics attached.',
              'kind': 'bug', 'meta': json.dumps(meta)}
    boundary = 'okilum-sprint-' + uuid.uuid4().hex
    body = bytearray()
    for name, value in fields.items():
        body.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{redact(value)}\r\n'.encode())
    body.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="log"; filename="sprint-report.txt"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n'.encode())
    body.extend(log.encode())
    body.extend(f'\r\n--{boundary}--\r\n'.encode())
    request = urllib.request.Request(endpoint, data=body, headers={'Content-Type': f'multipart/form-data; boundary={boundary}'})
    # No redirects: diagnostics must only go to the explicitly selected endpoint.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args):
            return None
    with urllib.request.build_opener(NoRedirect).open(request, timeout=20) as response:
        if response.status != 201:
            raise ValueError(f'Unexpected feedback HTTP {response.status}')
        receipt = json.loads(response.read(16385))
    expected = f"https://git.oklabs.uk/BeFeast/okilum/issues/{int(receipt['issue_number'])}"
    if receipt['issue_url'] != expected:
        raise ValueError('Unexpected feedback issue URL')
    return receipt


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--context')
    parser.add_argument('--stage', default='manual')
    parser.add_argument('--collect', action='store_true')
    parser.add_argument('--app-log')
    parser.add_argument('--repair-cache')
    args = parser.parse_args()
    if args.repair_cache:
        repair_cache(args.repair_cache)
        return
    context = json.loads(Path(args.context).read_text())
    latest = Path(context['root']) / 'sprint-context.json'
    if args.collect and latest.is_file():
        candidate = json.loads(latest.read_text())
        if candidate.get('sha') == context.get('sha'): context = candidate
    log = collect(context, args.stage, args.app_log)
    if args.collect:
        print(json.dumps({'log': log}))
        return
    report_path = Path.home() / 'Library/Logs/OkilumSprint/last-sprint-report.txt'
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(log)
    report_path.chmod(0o600)
    try:
        receipt = send(context, args.stage, log)
        print('Sprint report: ' + receipt['issue_url'])
    except Exception as error:
        print(f'Sprint report could not be delivered ({type(error).__name__}); kept at {report_path}', file=__import__('sys').stderr)
        raise SystemExit(1)


if __name__ == '__main__':
    main()
SPRINT_REPORTER
write_context() {
    python3 - "$context" "$root" "$app" "$branch" "$sha" "$stage" "$((SECONDS - started))" "$((SECONDS - stage_started))" "$log" "$run_log" <<'CONTEXT'
import json, pathlib, sys
path, root, app, branch, sha, stage, total, elapsed, log, run_log = sys.argv[1:]
p = pathlib.Path(path)
previous = json.loads(p.read_text()) if p.exists() else {}
durations = previous.get('durations', {})
durations[stage] = int(elapsed)
p.write_text(json.dumps(dict(root=root, app=app, branch=branch, sha=sha, stage=stage,
                            duration_seconds=int(total), durations=durations,
                            build_log=log, stage_log=run_log)))
CONTEXT
}
set_stage() {
    write_context
    stage=$1
    stage_started=$SECONDS
    write_context
    printf '\nSprint stage: %s\n' "$stage"
}
restore_sprint_app() {
    if (( restore_on_failure )); then
        rm -rf "$app"
        if [[ -e $backup ]]; then mv "$backup" "$app"; fi
        restore_on_failure=0
        echo 'Rejected Sprint installation removed; previous app restored when available.'
    fi
}
sprint_exit() {
    result=$?
    trap - EXIT INT TERM
    if (( result != 0 )); then
        write_context || true
        python3 "$reporter" --context "$context" --stage "$stage" || true
        # Collect the rejected bundle FIRST; only then restore the working app.
        restore_sprint_app || true
    fi
    if (( lock_owned )); then rmdir "$root/build.lock" || true; fi
    rm -rf "$bootstrap"
    exit "$result"
}
trap sprint_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
write_context
exec > >(tee -a "$run_log") 2>&1
if [[ -f $HOME/.cargo/env ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null; then
    echo "Install Rust, then rerun: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi
repo=https://git.oklabs.uk/BeFeast/okilum.git
git check-ref-format --branch "$branch" >/dev/null
root="${OKILUM_SPRINT_HOME:-$HOME/Developer/personal/okilum-sprint}"
root=${root%/}
if [[ $root != /* || $root == / || $root == "$HOME" ]]; then
    echo 'OKILUM_SPRINT_HOME must be an absolute, dedicated directory.' >&2; exit 1
fi
# Refuse symlinked workspaces/destinations rather than affecting another checkout.
for path in "$root" "$root/src" "$root/target" "$app" "$HOME/Applications"; do
    if [[ -L $path ]]; then echo "Refusing symlink: $path" >&2; exit 1; fi
done
legacy="$HOME/okilum-sprint"
if [[ ! -e $root && -e $legacy && $root != "$legacy" ]]; then
    if [[ -L $legacy || -L $legacy/src || -L $legacy/target || ! -d $legacy || -e $legacy/build.lock ]]; then
        echo "Cannot migrate $legacy: expected an idle, real Sprint directory." >&2; exit 1
    fi
    mkdir -p "$(dirname "$root")"
    mv "$legacy" "$root"
    printf 'Moved Sprint checkout and warm cache: %s → %s\n' "$legacy" "$root"
fi
mkdir -p "$root" "$HOME/Applications"
if ! mkdir "$root/build.lock" 2>/dev/null; then
    echo "Sprint build is already running (lock: $root/build.lock)." >&2; exit 1
fi
lock_owned=1
log="$root/build.log"
# Process-substitution tee may not have created the bootstrap log yet.
# Missing diagnostics must not replace the actual fetch/build failure status.
if [[ -f $run_log ]]; then
    cp "$run_log" "$root/stages.log"
else
    : > "$root/stages.log"
fi
run_log="$root/stages.log"
exec > >(tee -a "$run_log") 2>&1
set_stage fetch
if [[ ! -e $root/src ]]; then git clone "$repo" "$root/src"; fi
cd "$root/src"
[[ $(git rev-parse --show-toplevel) == "$root/src" ]] || { echo 'Not the dedicated sprint checkout.' >&2; exit 1; }
[[ $(git remote get-url origin) == "$repo" ]] || { echo 'Unexpected sprint origin.' >&2; exit 1; }
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
    echo "Sprint checkout has local changes; preserve them before rebuilding: $root/src" >&2; exit 1
fi
git fetch origin "refs/heads/$branch"
git checkout -B sprint FETCH_HEAD
sha=$(git rev-parse HEAD)
short_sha=$(git rev-parse --short HEAD)
export CC=/usr/bin/clang CXX=/usr/bin/clang++
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
export SDKROOT
export CARGO_TARGET_DIR="$root/target" CARGO_INCREMENTAL=1
export OKILUM_SOURCE_COMMIT="$sha" OKILUM_BUILD_VERSION=1
export OKILUM_RELEASE_VERSION="Sprint $short_sha"
set_stage vendor
python3 "$reporter" --repair-cache "$root"
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
# The binary links Sparkle, but this app has no feed or updater controller.
# Reuse the release lane's pinned framework loader, not a new download URL.
archive="$root/Sparkle.tar.xz"
if [[ ! -f $archive ]]; then python3 scripts/updater/sparkle.py fetch "$archive"; fi
python3 scripts/updater/sparkle.py prepare --archive "$archive" --destination vendor/sparkle
set_stage build
if ! cargo build --locked --profile sprint --target aarch64-apple-darwin -p okilum-shell 2>&1 | tee "$log"; then
    echo "Sprint compile failed; first compiler error (full log: $log):" >&2
    awk '/^error/ {print; n=8} n>0 {if ($0 !~ /^error/) print; n--}' "$log" | head -25 >&2 || true
    exit 1
fi
set_stage assemble-sign
bundle_stage="$root/Okilum Sprint.app"
rm -rf "$bundle_stage"
mkdir -p "$bundle_stage/Contents/MacOS" "$bundle_stage/Contents/Resources" "$bundle_stage/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/sprint/okilum" "$bundle_stage/Contents/MacOS/okilum"
ditto vendor/sparkle/Sparkle.framework "$bundle_stage/Contents/Frameworks/Sparkle.framework"
python3 scripts/third-party-notices.py --stage "$bundle_stage/Contents/Resources/Licenses"
# Apply isolation on every launch, including Finder launches after this terminal closes.
cat > "$bundle_stage/Contents/MacOS/sprint-launch" <<'LAUNCH'
#!/bin/bash
set -euo pipefail
export OKILUM_STATE_DIR="$HOME/Library/Application Support/com.befeast.okilum.sprint"
export XDG_CONFIG_HOME="$OKILUM_STATE_DIR/config"
export XDG_CACHE_HOME="$OKILUM_STATE_DIR/cache"
export XDG_STATE_HOME="$OKILUM_STATE_DIR/state"
export XDG_DATA_HOME="$OKILUM_STATE_DIR/data"
mkdir -p "$OKILUM_STATE_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME"
cd "$(dirname "$0")"
export OKILUM_SPRINT_REPORT_HELPER="$PWD/../Resources/sprint-report.py"
export OKILUM_SPRINT_CONTEXT="$PWD/../Resources/sprint-context.json"
exec ./okilum "$@"
LAUNCH
chmod +x "$bundle_stage/Contents/MacOS/sprint-launch"
# Reuse the canonical bundle/Info.plist template from build-macos-ci.sh.
python3 - "$bundle_stage" "$sha" <<'PY'
import pathlib, plistlib, sys
stage, sha = pathlib.Path(sys.argv[1]), sys.argv[2]
source = pathlib.Path('scripts/build-macos-ci.sh').read_text()
template = source.split('cat >"$APP/Contents/Info.plist" <<PLIST\n', 1)[1].split('\nPLIST\n', 1)[0]
for key, value in {'BUNDLE_ID':'com.befeast.okilum.sprint', 'DISPLAY_VERSION':'0.1.1',
                   'OKILUM_BUILD_VERSION':'1', 'SOURCE_SHA':sha, 'SOURCE_TREE':''}.items():
    template = template.replace('$' + key, value)
info = plistlib.loads(template.encode())
for key in list(info):
    if key.startswith('SU'):
        del info[key]
# Accept explicit Dock/Finder drops without becoming a default document handler.
for document in info.get('CFBundleDocumentTypes', []):
    document['LSHandlerRank'] = 'None'
# Keep the working app's URL protocol exclusive to the working app.
info.pop('CFBundleURLTypes', None)
for service in info.get('NSServices', []):
    service['NSPortName'] = 'Okilum Sprint'
info.update(CFBundleExecutable='sprint-launch', CFBundleName='Okilum Sprint',
            CFBundleDisplayName='Okilum Sprint', SUEnableAutomaticChecks=False,
            NSHumanReadableCopyright=f'Sprint source {sha}')
(stage / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
PY
# No LaunchServices environment injection; the launcher sets process env.
# All bundle metadata/resources are finalized BEFORE signing.
cp "$reporter" "$bundle_stage/Contents/Resources/sprint-report.py"
write_context
cp "$context" "$bundle_stage/Contents/Resources/sprint-context.json"
plutil -lint "$bundle_stage/Contents/Info.plist"
# Reuse the approved icon rasterizer; add a bright amber sprint badge.
env DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle" cargo run --locked --profile sprint \
    --target aarch64-apple-darwin -p okilum-shell --example macos_app_icon -- "$root/icon-rasters"
python3 - "$root/icon-rasters" <<'PY'
from pathlib import Path
import sys
for path in Path(sys.argv[1]).glob('*.rgba'):
    size = int(path.stem)
    rgba = bytearray(path.read_bytes())
    for y in range(size * 2 // 3, size * 15 // 16):
        for x in range(size * 2 // 3, size * 15 // 16):
            offset = (y * size + x) * 4
            rgba[offset:offset + 4] = bytes((255, 176, 0, 255))
    path.write_bytes(rgba)
PY
python3 scripts/macos-icon.py "$root/icon-rasters" "$bundle_stage/Contents/Resources/Okilum.icns"
codesign --force --deep --sign - "$bundle_stage"
codesign --verify --deep --strict "$bundle_stage"
# Target only the Sprint bundle ID. Quit must finish before replacing its bundle.
if [[ -d $app ]]; then
    osascript -e 'if application id "com.befeast.okilum.sprint" is running then tell application id "com.befeast.okilum.sprint" to quit'
    for ((attempt=0; attempt<15; attempt++)); do
        running=$(osascript -e 'application id "com.befeast.okilum.sprint" is running')
        [[ $running == false ]] && break
        sleep 1
    done
    [[ $running == false ]] || { echo 'Quit Okilum Sprint, then rerun; app was not replaced.' >&2; exit 1; }
fi
# Only these dedicated Sprint paths can be replaced; keep the previous app on failure.
backup="$root/previous-Okilum-Sprint.app"
rm -rf "$backup"
if [[ -e $app ]]; then mv "$app" "$backup"; fi
restore_on_failure=1
ditto "$bundle_stage" "$app"
printf 'Launching installed Sprint app: %s (source %s)\n' "$app" "$sha"
plutil -lint "$app/Contents/Info.plist"
codesign --verify --deep --strict -v "$app"
set_stage launch
open "$app"
restore_on_failure=0
write_context
cp "$context" "$root/sprint-context.json"
printf 'Sprint ready: %s %s · %ss · %s\n' "$branch" "$short_sha" "$((SECONDS - started))" "$app"
