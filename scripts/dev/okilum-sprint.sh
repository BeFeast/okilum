#!/bin/bash
# Local-only Mac sprint build. Run in Terminal, never through the release lane.
# From the one checkout: bash <(git show origin/main:scripts/dev/okilum-sprint.sh) <branch>
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
# One app (Oleg, 2026-11-04 #2589): the fast local build replaces the working Okilum.
app="/Applications/Okilum.app"
bundle_stage=
STAGE_DIR=staging.noindex
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
log="$bootstrap/build.log"
run_log="$bootstrap/stages.log"
cat > "$reporter" <<'SPRINT_REPORTER'
#!/usr/bin/env python3
"""Bounded Sprint diagnostics; no clipboard API, no credentials, no retries."""
import argparse
from decimal import Decimal, InvalidOperation
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import xml.etree.ElementTree as ElementTree

ENDPOINT = 'https://pomoi.co/api/feedback/okilum'
MAX_LOG = 250 * 1024
SPARKLE = '{http://www.andymatuschak.org/xml-namespaces/sparkle}'
# Staged and previous bundles live here: Spotlight skips *.noindex, so
# LaunchServices does not register them next to /Applications/Okilum.app.
STAGE_DIR = 'staging.noindex'


def next_build(appcast):
    """One above every build in the appcast: Sparkle offers no older beta over
    this build and offers the next newer one. Returns (build, newest, newest beta)."""
    builds, betas = [], []
    for item in ElementTree.fromstring(appcast).iter('item'):
        version = item.findtext(SPARKLE + 'version')
        enclosure = item.find('enclosure')
        if version is None and enclosure is not None:
            version = enclosure.get(SPARKLE + 'version')
        if version and version.strip().isdigit():
            builds.append(int(version.strip()))
            if (item.findtext(SPARKLE + 'channel') or '').strip() == 'beta':
                betas.append(int(version.strip()))
    if not builds:
        raise ValueError('no published Mac build in the update feed')
    return max(builds) + 1, max(builds), max(betas) if betas else 0


def registered_copies(dump, bundle_id, keep):
    """Bundles `lsregister -dump` lists for bundle_id other than keep (or inside it)."""
    keep = os.path.normpath(keep)
    copies = []
    for block in re.split(r'(?m)^-{20,}\s*$', dump):
        identifier = re.search(r'(?m)^\s*identifier:\s*(\S+)\s*$', block)
        path = re.search(r'(?m)^\s*path:\s*(.+?)(?:\s+\(0x[0-9a-fA-F]+\))?\s*$', block)
        if not identifier or not path or identifier.group(1) != bundle_id:
            continue
        candidate = os.path.normpath(path.group(1))
        if (candidate.endswith('.app') and candidate != keep and not candidate.startswith(keep + '/')
                and candidate not in copies):
            copies.append(candidate)
    return copies


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
    # macOS temp/workspace paths can name the same root through /var and
    # /private/var. Such aliases do not make a warm build output stale.
    old = {value for origin in [previous, str(Path.home() / 'okilum-sprint')]
           if origin and Path(origin).resolve() != root
           for value in [origin, str(Path(origin).resolve())]}
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
            stale = any(path in text for path in old) or any(
                not Path(path).resolve().is_relative_to(root / 'src') for path in icons)
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


def trim_target(root):
    """Bound only the dedicated Mac Sprint profile while its parent owns the lock."""
    root = Path(root)
    if not root.is_absolute() or root == Path('/') or root == Path.home():
        raise ValueError('Sprint cache requires an absolute dedicated root')
    target = root / 'target'
    profile = target / 'aarch64-apple-darwin/sprint'
    lock = root / 'build.lock'
    owner = lock / 'owner'
    for path in [root, target, target / 'aarch64-apple-darwin', profile,
                 profile / 'incremental', lock, owner]:
        if path.is_symlink():
            raise ValueError(f'Refusing Sprint cache symlink: {path}')
    if owner.read_text().split(maxsplit=1)[0] != str(os.getppid()):
        raise ValueError('Sprint cache cleanup requires the parent build.lock')
    try:
        cap = Decimal(os.environ.get('OKILUM_SPRINT_TARGET_CAP_GIB', '20'))
        if not cap.is_finite() or cap <= 0 or cap * 1024**2 < 1:
            raise ValueError('Sprint target cap must be at least 1 KiB')
        cap_kib = int(cap * 1024**2)
    except InvalidOperation as error:
        raise ValueError('OKILUM_SPRINT_TARGET_CAP_GIB must be a positive GiB value') from error

    def size():
        if not target.exists():
            return 0
        return int(subprocess.check_output(['du', '-sk', str(target)], text=True).split()[0])

    free = shutil.disk_usage(target if target.exists() else root).free / 1024**3
    print(f'Sprint target volume: free={free:.2f} GiB' +
          ('; WARNING below 50 GiB' if free < 50 else ''), flush=True)
    before = after = size()
    removed = []
    for path in [profile / 'incremental', profile]:
        if after <= cap_kib:
            break
        if path.is_dir():
            # The profile's parents are not symlinks; rmtree does not follow
            # symlinks nested in either disposable directory.
            shutil.rmtree(path)
            removed.append(str(path.relative_to(target)))
            after = size()
    # Cargo can recreate the empty scaffold even with incremental compilation
    # disabled. Remove only that empty directory under the same build lock;
    # an existing populated warm cache still follows the cap policy above.
    incremental = profile / 'incremental'
    if (os.environ.get('CARGO_INCREMENTAL') == '0' and incremental.is_dir()
            and not any(incremental.iterdir())):
        incremental.rmdir()
        removed.append(str(incremental.relative_to(target)) + '(empty)')
        after = size()
    print(f'Sprint target: before_kib={before} after_kib={after} cap_gib={cap} '
          f'removed={",".join(removed) or "none"}', flush=True)
    if after > cap_kib:
        raise ValueError('Sprint target is still over cap outside the allowed Mac Sprint profile; '
                         'build stopped without deleting other target outputs')


def collect(context, stage, app_log=None):
    app = Path(context['app'])
    sections = []
    def add(name, value):
        sections.append(f'\n=== {name} ===\n' + str(value))
    add('Sprint context', json.dumps(context, sort_keys=True))
    add('Stage log (last 200 lines)', '\n'.join(tail(context.get('stage_log', context['build_log']))))
    add('Build log (last 200 lines)', '\n'.join(tail(context['build_log'])))
    add('codesign staged bundle', run(['codesign', '--verify', '--deep', '--strict', '-v', str(Path(context['root']) / STAGE_DIR / 'Okilum.app')]))
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
    parser.add_argument('--trim-target')
    parser.add_argument('--next-build', action='store_true', help='appcast on stdin')
    parser.add_argument('--registered-copies', nargs=2, metavar=('BUNDLE_ID', 'KEEP'), help='dump on stdin')
    args = parser.parse_args()
    if args.trim_target:
        trim_target(args.trim_target)
        return
    if args.repair_cache:
        repair_cache(args.repair_cache)
        return
    if args.next_build:
        print(*next_build(sys.stdin.buffer.read()))
        return
    if args.registered_copies:
        for copy in registered_copies(sys.stdin.read(), *args.registered_copies):
            print(copy)
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
        if [[ -x ${lsregister:-} ]]; then "$lsregister" -f "$app" >/dev/null 2>&1 || true; fi
        restore_on_failure=0
        echo 'Rejected Sprint installation removed; previous app restored when available.'
    fi
}
# Bundle copies must not stay registered next to /Applications/Okilum.app: with
# several com.befeast.okilum bundles SMAppService and Sync helper registration
# pick the wrong one (04.11). Best effort, never a build failure.
unregister() {
    local path
    [[ -x $lsregister ]] || return 0
    for path in "$@"; do "$lsregister" -u "$path" >/dev/null 2>&1 || true; done
}
keep_one_registration() {
    local copies copy
    if [[ ! -x $lsregister ]]; then echo 'lsregister not found: LaunchServices left as is.'; return 0; fi
    "$lsregister" -f "$app" >/dev/null 2>&1 || true
    copies=$("$lsregister" -dump 2>/dev/null | python3 "$reporter" --registered-copies "$SPRINT_BUNDLE_ID" "$app") || copies=
    while IFS= read -r copy; do
        [[ -n $copy ]] || continue
        "$lsregister" -u "$copy" >/dev/null 2>&1 || true
        printf 'LaunchServices: unregistered %s\n' "$copy"
    done <<< "$copies"
}
# A lock left by a build that died (crash, closed Terminal, reboot) is taken
# over; a live build keeps it. Owner = pid + start time, so a reused pid is stale.
process_start() { ps -o lstart= -p "$1" 2>/dev/null | sed 's/^ *//;s/ *$//'; }
acquire_lock() {
    local lock=$1 owner pid since age
    if mkdir "$lock" 2>/dev/null; then
        printf '%s %s\n' "$$" "$(process_start $$)" > "$lock/owner"
        return 0
    fi
    owner=$(cat "$lock/owner" 2>/dev/null || true)
    pid=${owner%% *}
    since=${owner#* }
    if [[ -n $owner ]]; then
        if [[ $pid =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null && [[ $(process_start "$pid") == "$since" ]]; then
            echo "A build is already running (pid $pid, lock $lock)." >&2
            return 1
        fi
    else
        # No owner record: a lock from an older script version, or one created
        # this instant. Only an old one can be stale.
        age=$(($(date +%s) - $(stat -c %Y "$lock" 2>/dev/null || stat -f %m "$lock" 2>/dev/null || date +%s)))
        if ((age < 7200)); then
            echo "A build may be running (lock $lock without an owner, ${age}s old). Remove it if none is." >&2
            return 1
        fi
    fi
    # Rename first: of two builds that both saw a stale lock, only one wins.
    mv "$lock" "$lock.stale.$$" 2>/dev/null || { echo "Lock $lock changed hands; retry." >&2; return 1; }
    rm -rf "$lock.stale.$$"
    echo "Removed a stale build lock (${owner:-no owner})."
    mkdir "$lock" 2>/dev/null || { echo "Another build took $lock." >&2; return 1; }
    printf '%s %s\n' "$$" "$(process_start $$)" > "$lock/owner"
}
# The script runs from origin/main against any branch (README): a branch from
# before okilum-macos-bundle-tools (#2880) still has the old examples, which
# link dev-dependencies but produce the same bytes. Prints the cargo argv.
sprint_bundle_tool() {
    local tool=$1
    if [[ -f crates/okilum-macos-bundle-tools/Cargo.toml ]]; then
        printf '%s\n' cargo run --locked --profile sprint --target aarch64-apple-darwin \
            -p okilum-macos-bundle-tools --bin "$tool" --
    elif [[ $tool == okilum-launch-agent-plist ]]; then
        printf '%s\n' cargo run --locked --profile sprint --target aarch64-apple-darwin \
            -p okilum-sync-controller --example bundled_plist --
    else
        printf '%s\n' env DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle" cargo run --locked --profile sprint \
            --target aarch64-apple-darwin -p okilum-shell --example macos_app_icon --
    fi
}
sprint_exit() {
    result=$?
    trap - EXIT INT TERM
    if (( result != 0 )); then
        # Visible even when the caller pipes into tee, which reports tee's status.
        printf '\nSprint FAILED at stage %s (exit %s)\n' "$stage" "$result" >&2
        write_context || true
        python3 "$reporter" --context "$context" --stage "$stage" || true
        # Collect the rejected bundle FIRST; only then restore the working app.
        restore_sprint_app || true
        if [[ -n $bundle_stage ]]; then unregister "$bundle_stage" || true; fi
    fi
    if (( lock_owned )); then rm -f "$root/build.lock/owner"; rmdir "$root/build.lock" || true; fi
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
# One repository (Oleg, 2026-11-05): the build runs in a git worktree of his
# own checkout, never in a separate clone. Its origin is the Forgejo SSH remote;
# any SSH or HTTPS spelling of the same repository is accepted.
checkout="${OKILUM_CHECKOUT:-$HOME/Developer/personal/okilum}"
checkout=${checkout%/}
origin_pattern='^((ssh://)?git@(git\.oklabs\.uk|10\.10\.0\.27)(:2222)?[:/]|https://git\.oklabs\.uk/)BeFeast/okilum(\.git)?$'
git check-ref-format --branch "$branch" >/dev/null
# $root/src is the worktree; $root/target keeps the warm cargo cache.
root="${OKILUM_SPRINT_HOME:-$HOME/Developer/worktrees/okilum/sprint}"
root=${root%/}
if [[ $root != /* || $root == / || $root == "$HOME" ]]; then
    echo 'OKILUM_SPRINT_HOME must be an absolute, dedicated directory.' >&2; exit 1
fi
# Refuse symlinked workspaces/destinations rather than affecting another checkout.
for path in "$root" "$root/src" "$root/target" "$root/$STAGE_DIR" "$app"; do
    if [[ -L $path ]]; then echo "Refusing symlink: $path" >&2; exit 1; fi
done
# Earlier versions kept a separate clone; move its warm cache here and delete it.
for legacy in "$HOME/Developer/personal/okilum-sprint" "$HOME/okilum-sprint"; do
    [[ $legacy != "$root" && -d $legacy && ! -L $legacy ]] || continue
    if [[ -d $legacy/src ]] && [[ -n $(git -C "$legacy/src" status --porcelain 2>/dev/null) ]]; then
        echo "Old Sprint clone left in place (local changes): $legacy" >&2; continue
    fi
    # A lock whose build died (the 04.11 one) must not keep the old clone forever.
    if [[ -e $legacy/build.lock ]] && ! acquire_lock "$legacy/build.lock"; then
        echo "Old Sprint clone left in place (a build is running there): $legacy" >&2; continue
    fi
    mkdir -p "$root"
    if [[ ! -e $root/target && -d $legacy/target && ! -L $legacy/target ]]; then mv "$legacy/target" "$root/target"; fi
    if [[ ! -e $root/Sparkle.tar.xz && -f $legacy/Sparkle.tar.xz ]]; then mv "$legacy/Sparkle.tar.xz" "$root/"; fi
    # Its staged and previous apps carry com.befeast.okilum: unregister before deleting.
    unregister "$legacy/Okilum Sprint.app" "$legacy/previous-Okilum-Sprint.app"
    rm -rf "$legacy"
    printf 'Removed old Sprint clone %s; warm cache moved to %s\n' "$legacy" "$root"
done
mkdir -p "$root"
acquire_lock "$root/build.lock" || exit 1
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
[[ -e $checkout/.git ]] || {
    echo "Okilum checkout not found: $checkout. Clone it once over SSH (git clone ssh://git@10.10.0.27:2222/BeFeast/okilum.git $checkout) or set OKILUM_CHECKOUT." >&2; exit 1; }
origin=$(git -C "$checkout" remote get-url origin)
[[ $origin =~ $origin_pattern ]] || { echo "Unexpected origin in $checkout: $origin" >&2; exit 1; }
git -C "$checkout" fetch origin "+refs/heads/$branch:refs/remotes/origin/$branch"
if [[ ! -e $root/src ]]; then
    git -C "$checkout" worktree prune
    git -C "$checkout" worktree add --detach "$root/src" "refs/remotes/origin/$branch"
fi
cd "$root/src"
[[ $(git rev-parse --show-toplevel) == "$root/src" ]] || { echo 'Not the Sprint worktree.' >&2; exit 1; }
common=$(git rev-parse --path-format=absolute --git-common-dir)
[[ $common == "$(git -C "$checkout" rev-parse --path-format=absolute --git-common-dir)" ]] || {
    echo "$root/src is not a worktree of $checkout; remove it and rerun." >&2; exit 1; }
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
    echo "Sprint worktree has local changes; preserve them before rebuilding: $root/src" >&2; exit 1
fi
git checkout --detach "refs/remotes/origin/$branch"
sha=$(git rev-parse HEAD)
short_sha=$(git rev-parse --short HEAD)
export CC=/usr/bin/clang CXX=/usr/bin/clang++
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
export SDKROOT
export CARGO_TARGET_DIR="$root/target" CARGO_INCREMENTAL="${CARGO_INCREMENTAL-1}"
# One above every published Mac build: Sparkle neither puts an older beta over
# this build nor holds back the next newer beta (a fixed number did both, 04.11).
if [[ -n ${OKILUM_SPRINT_BUILD:-} ]]; then
    [[ $OKILUM_SPRINT_BUILD =~ ^[1-9][0-9]*$ ]] || { echo 'OKILUM_SPRINT_BUILD must be a number.' >&2; exit 1; }
    build=$OKILUM_SPRINT_BUILD
    printf 'Build %s (OKILUM_SPRINT_BUILD)\n' "$build"
else
    appcast=$(curl -fsSL -A curl --max-time 30 https://updates.befeast.com/okilum/appcast.xml) || {
        echo 'Cannot read the Mac update feed; set OKILUM_SPRINT_BUILD to a number above the newest beta and rerun.' >&2; exit 1; }
    versions=$(printf '%s' "$appcast" | python3 "$reporter" --next-build)
    read -r build newest newest_beta <<< "$versions"
    printf 'Build %s: one above the newest published Mac build %s (newest beta %s).\n' "$build" "$newest" "$newest_beta"
fi
export OKILUM_SOURCE_COMMIT="$sha" OKILUM_BUILD_VERSION="$build"
export OKILUM_RELEASE_VERSION="0.1.$build"
# Sync needs the Developer ID-signed helper and the bundled Syncthing runtime;
# the app compiles this team and bundle into its helper policy (#2589).
SPRINT_BUNDLE_ID=com.befeast.okilum
SPRINT_IDENTITY="${OKILUM_SIGNING_IDENTITY:-Developer ID Application: Oleg Kossoy (LA9V2K2376)}"
source scripts/ci/supervisor-bundle.sh
SPRINT_TEAM_ID=$(supervisor_team_id "$SPRINT_IDENTITY")
export OKILUM_SIGNING_TEAM_ID="$SPRINT_TEAM_ID" OKILUM_BUNDLE_ID="$SPRINT_BUNDLE_ID"
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
python3 "$reporter" --trim-target "$root"
if ! cargo build --locked --profile sprint --target aarch64-apple-darwin -p okilum-shell 2>&1 | tee "$log"; then
    echo "Sprint compile failed; first compiler error (full log: $log):" >&2
    awk '/^error/ {print; n=8} n>0 {if ($0 !~ /^error/) print; n--}' "$log" | head -25 >&2 || true
    exit 1
fi
# Same warm incremental sprint profile as the app: no release rebuilds.
cargo build --locked --profile sprint --target aarch64-apple-darwin -p okilum-sync-supervisor
set_stage assemble-sign
# Copies from before 05.11 sat outside a .noindex directory and stayed registered.
for legacy_copy in "$root/Okilum Sprint.app" "$root/previous-Okilum-Sprint.app"; do
    if [[ -e $legacy_copy ]]; then unregister "$legacy_copy"; rm -rf "$legacy_copy"; fi
done
bundle_stage="$root/$STAGE_DIR/Okilum.app"
rm -rf "$bundle_stage"
mkdir -p "$bundle_stage/Contents/MacOS" "$bundle_stage/Contents/Resources" "$bundle_stage/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/sprint/okilum" "$bundle_stage/Contents/MacOS/okilum"
install -m 755 "$CARGO_TARGET_DIR/aarch64-apple-darwin/sprint/$SUPERVISOR_NAME" "$bundle_stage/$SUPERVISOR_RELATIVE"
mkdir -p "$bundle_stage/Contents/Library/LaunchAgents"
tool_command=(); while IFS= read -r word; do tool_command+=("$word"); done < <(sprint_bundle_tool okilum-launch-agent-plist)
"${tool_command[@]}" "$SUPERVISOR_RELATIVE" "$bundle_stage/Contents/Library/LaunchAgents"
# The Mac has no gpg (#2589): the pinned archive SHA256 still gates the bytes;
# only the upstream signature check is skipped on a host without gpg.
python3 - "$bundle_stage" <<'PY'
import importlib.util, shutil, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("bundle", "scripts/syncthing/bundle.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)
if shutil.which("gpg") is None:
    bundle.verify_upstream = lambda: {"skipped": "gpg unavailable; archive SHA256 pin enforced"}
bundle.stage(Path(sys.argv[1]), "arm64", None)
PY
ditto vendor/sparkle/Sparkle.framework "$bundle_stage/Contents/Frameworks/Sparkle.framework"
python3 scripts/third-party-notices.py --stage "$bundle_stage/Contents/Resources/Licenses"
# Reuse the canonical bundle/Info.plist template from build-macos-ci.sh.
python3 - "$bundle_stage" "$sha" "$build" <<'PY'
import pathlib, plistlib, sys
stage, sha, build = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
source = pathlib.Path('scripts/build-macos-ci.sh').read_text()
template = source.split('cat >"$APP/Contents/Info.plist" <<PLIST\n', 1)[1].split('\nPLIST\n', 1)[0]
for key, value in {'BUNDLE_ID':'com.befeast.okilum', 'DISPLAY_VERSION':'0.1.' + build,
                   'OKILUM_BUILD_VERSION':build, 'SOURCE_SHA':sha, 'SOURCE_TREE':'',
                   'SPARKLE_PUBLIC_ED_KEY':'TH/Jc6vRfSSFtR3R2rLqBGBnFylMUWdKJ6dr18Ys8CY='}.items():
    template = template.replace('$' + key, value)
info = plistlib.loads(template.encode())
(stage / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
PY
# No LaunchServices environment injection; the launcher sets process env.
# All bundle metadata/resources are finalized BEFORE signing.
cp "$reporter" "$bundle_stage/Contents/Resources/sprint-report.py"
write_context
cp "$context" "$bundle_stage/Contents/Resources/sprint-context.json"
plutil -lint "$bundle_stage/Contents/Info.plist"
# Reuse the approved icon rasterizer; add a bright amber sprint badge.
tool_command=(); while IFS= read -r word; do tool_command+=("$word"); done < <(sprint_bundle_tool okilum-app-icon-rasters)
"${tool_command[@]}" "$root/icon-rasters"
python3 scripts/macos-icon.py "$root/icon-rasters" "$bundle_stage/Contents/Resources/Okilum.icns"
# Inside-out Developer ID signing, as in scripts/updater/sign-bundle.sh, without
# notarization: Sync only trusts a helper and runtime signed by this team.
framework="$bundle_stage/Contents/Frameworks/Sparkle.framework"
for component in \
    "$framework/Versions/B/XPCServices/Downloader.xpc" \
    "$framework/Versions/B/XPCServices/Installer.xpc" \
    "$framework/Versions/B/Updater.app" \
    "$framework/Versions/B/Autoupdate" \
    "$framework"; do
    codesign --force --preserve-metadata=identifier,entitlements --options runtime --sign "$SPRINT_IDENTITY" "$component"
done
supervisor_sign "$bundle_stage" "$SPRINT_IDENTITY" "$SPRINT_BUNDLE_ID"
codesign --force --options runtime --sign "$SPRINT_IDENTITY" "$bundle_stage"
codesign --verify --deep --strict "$bundle_stage"
# Cargo is finished and every executable is staged. Keep the completed build
# under the same cap too, without deleting a binary still needed for assembly.
python3 "$reporter" --trim-target "$root"
unregister "$bundle_stage"
# Target only the Sprint bundle ID. Quit must finish before replacing its bundle.
if [[ -d $app ]]; then
    osascript -e 'if application id "com.befeast.okilum" is running then tell application id "com.befeast.okilum" to quit'
    for ((attempt=0; attempt<15; attempt++)); do
        running=$(osascript -e 'application id "com.befeast.okilum" is running')
        [[ $running == false ]] && break
        sleep 1
    done
    [[ $running == false ]] || { echo 'Quit Okilum, then rerun; app was not replaced.' >&2; exit 1; }
fi
# Only these dedicated Sprint paths can be replaced; keep the previous app on failure.
backup="$root/$STAGE_DIR/previous-Okilum.app"
rm -rf "$backup"
# A moved bundle keeps its LaunchServices record: unregister the backup at once.
if [[ -e $app ]]; then mv "$app" "$backup"; unregister "$backup"; fi
restore_on_failure=1
ditto "$bundle_stage" "$app"
printf 'Launching installed Sprint app: %s (source %s)\n' "$app" "$sha"
plutil -lint "$app/Contents/Info.plist"
codesign --verify --deep --strict -v "$app"
unregister "$bundle_stage" "$backup"
keep_one_registration
set_stage launch
open "$app"
restore_on_failure=0
write_context
cp "$context" "$root/sprint-context.json"
printf 'Sprint ready: %s %s · Okilum 0.1.%s (build %s) · %ss · %s\n' "$branch" "$short_sha" "$build" "$build" "$((SECONDS - started))" "$app"
