#!/usr/bin/env bash
# Local Linux dev build (Oleg 05.11, #2589): build the AppImage of a branch or
# commit on this machine, put it over ~/.local/share/okilum/Okilum.AppImage
# (previous file kept as Okilum.AppImage.previous) and restart a running Okilum.
# Never a release: no feed, signing, upload or provenance. Docs: scripts/dev/README.md.
set -Eeuo pipefail
usage() {
    cat <<'USAGE'
Usage: okilum-dev-linux.sh [--no-restart] <branch|commit>

Builds the AppImage of <branch> (or of a commit on an origin branch) in a git
worktree of your Okilum checkout with a warm Cargo target, installs it over
~/.local/share/okilum/Okilum.AppImage and restarts a running Okilum.

Environment:
  OKILUM_CHECKOUT     the checkout to fetch in and add the worktree to (default:
                      the checkout this script is in; without one, a clone in
                      OKILUM_DEV_HOME/repo)
  OKILUM_DEV_REPO     remote for that clone (default https://git.oklabs.uk/BeFeast/okilum.git)
  OKILUM_DEV_HOME     worktree, Cargo target and logs (default ~/.cache/okilum-dev)
  OKILUM_DEV_TARGET_CAP_GIB  the Cargo target is emptied once it passes this many
                      GiB (default 30); inside a build-slot run the slot's target
                      is used and bounded by build-slot instead
  OKILUM_DEV_BUILD    build number (default: newest published Linux build + 1)
  OKILUM_DEV_PROFILE  Cargo profile (default release)

Exit: 0 installed (and restarted if it was running); 1 failure, nothing
installed; 3 installed, but the running Okilum did not quit (it keeps the old
build until you restart it).
USAGE
}
restart=1
args=()
while (($#)); do
    case $1 in
        --no-restart) restart=0 ;;
        -h | --help) usage; exit 0 ;;
        -*) usage >&2; exit 2 ;;
        *) args+=("$1") ;;
    esac
    shift
done
if [[ ${#args[@]} != 1 ]]; then usage >&2; exit 2; fi
ref=${args[0]}
if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
    echo 'okilum-dev-linux.sh builds x86_64 Linux AppImages only.' >&2; exit 1
fi
started=$SECONDS
umask 022
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
start_dir=$(pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/okilum-dev.XXXXXX")
helper="$work/okilum-dev-helper.py"
cat > "$helper" <<'DEV_HELPER'
#!/usr/bin/env python3
"""okilum-dev-linux.sh helper: origin, build number, GUI quit/launch. Stdlib only."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

# Set by the AppImage runtime or a launcher for one process; never inherited.
RUNTIME_ENV = ('APPDIR', 'APPIMAGE', 'ARGV0', 'OWD', 'DESKTOP_STARTUP_ID', 'XDG_ACTIVATION_TOKEN')
INSTANCE_LOCK = 'reader-instance.lock'
OWNER = b'Okilum managed AppImage v1\n'


def okilum_remote(url):
    tail = url.strip().rstrip('/')
    if tail.lower().endswith('.git'):
        tail = tail[:-4]
    return re.search(r'(^|[/:])befeast/okilum$', tail, re.I) is not None


def git(directory, *args):
    try:
        result = subprocess.run(['git', '-C', str(directory), *args], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return ''
    return result.stdout.strip() if result.returncode == 0 else ''


def checkout(environ, *directories):
    """One repository (Oleg 05.11): OKILUM_CHECKOUT, else the Okilum checkout around
    the script or the working directory (its main worktree), else '' and the
    caller clones one. `bash <(git show …)` leaves only the working directory."""
    explicit = environ.get('OKILUM_CHECKOUT', '').strip()
    if explicit:
        return explicit, 'OKILUM_CHECKOUT'
    for directory in directories:
        common = git(directory, 'rev-parse', '--path-format=absolute', '--git-common-dir')
        if not common:
            continue
        repository = str(Path(common).parent) if Path(common).name == '.git' else common
        if okilum_remote(git(repository, 'remote', 'get-url', 'origin')):
            return repository, 'Okilum checkout around ' + str(directory)
    return '', 'no Okilum checkout around ' + ' or '.join(str(d) for d in directories)


def next_build(feeds):
    """One above every published Linux build: no beta replaces it, the next one does."""
    builds = []
    for text in feeds:
        if text.strip():
            build = json.loads(text).get('build')
            if isinstance(build, int) and not isinstance(build, bool) and build > 0:
                builds.append(build)
    if not builds:
        raise ValueError('no published Linux build in the update feeds')
    return max(builds) + 1, max(builds)


def process_stat(proc, pid):
    """(state, start time) from /proc/<pid>/stat; comm may contain spaces and ')'."""
    fields = (Path(proc) / str(pid) / 'stat').read_text().rsplit(')', 1)[1].split()
    return fields[0], int(fields[19])


def environment(proc, pid):
    raw = (Path(proc) / str(pid) / 'environ').read_bytes()
    pairs = (item.split(b'=', 1) for item in raw.split(b'\0') if b'=' in item)
    return {k.decode(errors='surrogateescape'): v.decode(errors='surrogateescape') for k, v in pairs}


def gui_processes(proc='/proc', uid=None, lock=None):
    """Okilum windows: our processes whose executable is okilum and which hold the
    single-instance lock (any state directory, or exactly `lock`). Identified by
    /proc/<pid>/exe and fds, never argv: `okilum sync serve` runs the same
    executable without that lock."""
    uid = os.getuid() if uid is None else uid
    found = []
    for name in sorted(os.listdir(proc), key=lambda n: (len(n), n)):
        if not name.isdigit():
            continue
        base = Path(proc) / name
        try:
            if base.stat().st_uid != uid:
                continue
            exe = os.readlink(base / 'exe')
            if exe.endswith(' (deleted)'):
                exe = exe[:-len(' (deleted)')]
            if os.path.basename(exe) != 'okilum':
                continue
            holds = False
            for fd in os.listdir(base / 'fd'):
                try:
                    target = os.readlink(base / 'fd' / fd)
                except OSError:
                    continue
                target = target.split(' (deleted)')[0]
                if target == lock if lock else target.endswith('/' + INSTANCE_LOCK):
                    holds = True
                    break
            if not holds:
                continue
            env = environment(proc, name)
            found.append({'pid': int(name), 'start': process_stat(proc, name)[1], 'exe': exe,
                          'appimage': env.get('APPIMAGE'), 'env': env})
        except (OSError, ValueError, IndexError):
            continue
    return found


def alive(proc, process):
    """Same process (pid and start time) and not yet exited (a zombie has)."""
    try:
        state, start = process_stat(proc, process['pid'])
    except (OSError, ValueError, IndexError):
        return False
    return start == process['start'] and state not in ('Z', 'X')


def launch_env(env):
    return {k: v for k, v in env.items() if k not in RUNTIME_ENV}


def quit_gui(env_out, timeout, proc='/proc', lock=None, kill=os.kill, sleep=time.sleep, clock=time.monotonic):
    """0 quit (environment saved), 3 nothing to restart, 1 still running at the deadline."""
    running = gui_processes(proc, lock=lock)
    for process in running:
        if not process['appimage']:
            print(f"Okilum pid {process['pid']} runs {process['exe']}, not an AppImage: left running.")
    running = [p for p in running if p['appimage']]
    if not running:
        print('Okilum is not running: nothing to restart.')
        return 3
    Path(env_out).write_text(json.dumps(launch_env(running[0]['env'])))
    for process in running:
        if not alive(proc, process):
            continue
        # The app handles SIGTERM like Quit: saves, then exits (sync CLI --quit-app).
        print(f"Quitting Okilum pid {process['pid']} ({process['appimage']}): SIGTERM saves and exits.")
        try:
            kill(process['pid'], signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = clock() + timeout
    while any(alive(proc, p) for p in running):
        if clock() >= deadline:
            pids = ', '.join(str(p['pid']) for p in running if alive(proc, p))
            print(f'Okilum (pid {pids}) is still running after {timeout:g} s; '
                  'an open dialog may be waiting. It was not killed.')
            return 1
        sleep(0.25)
    return 0


def launch(image, env_file, log, wait, proc='/proc', lock=None):
    env = json.loads(Path(env_file).read_text())
    with open(log, 'ab') as output:
        child = subprocess.Popen([image], env=env, cwd=env.get('HOME') or '/',
                                 stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
                                 start_new_session=True, close_fds=True)
    deadline = time.monotonic() + wait
    while time.monotonic() < deadline:
        if child.poll() is not None:
            print(f'Okilum exited right after start (status {child.returncode}); log: {log}')
            return 1
        if any(p['pid'] == child.pid for p in gui_processes(proc, lock=lock)):
            print(f'Okilum restarted: pid {child.pid}')
            return 0
        time.sleep(0.25)
    print(f'Okilum started (pid {child.pid}) but holds no window lock after {wait:g} s; log: {log}')
    return 0


def record_build(image, build):
    """Okilum's own record of the build at the managed path (okilum-appimage
    installation.rs): without it an older downloaded AppImage may replace this one."""
    image = Path(image)
    marker = image.with_name('.appimage-owner')
    try:
        if marker.is_symlink() or marker.read_bytes() != OWNER:
            return False
        st = os.lstat(image)
    except OSError:
        return False
    text = f'{build} {st.st_size} {st.st_mtime_ns // 10**9} {st.st_mtime_ns % 10**9} {st.st_ino}'
    with tempfile.NamedTemporaryFile('w', dir=image.parent, prefix='.okilum-dev-record.', delete=False) as record:
        record.write(text)
        record.flush()
        os.fsync(record.fileno())
    os.replace(record.name, image.with_name('.appimage-build'))
    return True


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('checkout').add_argument('directories', nargs='+')
    sub.add_parser('okilum-remote').add_argument('url')
    sub.add_parser('next-build').add_argument('feeds', nargs='+')
    quit_parser = sub.add_parser('quit')
    quit_parser.add_argument('--env-out', required=True)
    quit_parser.add_argument('--timeout', type=float, default=30)
    launch_parser = sub.add_parser('launch')
    for name in ('--image', '--env', '--log'):
        launch_parser.add_argument(name, required=True)
    launch_parser.add_argument('--wait', type=float, default=30)
    record_parser = sub.add_parser('record-build')
    record_parser.add_argument('image')
    record_parser.add_argument('build', type=int)
    args = parser.parse_args()
    if args.command == 'checkout':
        # Two lines: the path may be empty, which a tab-split read would swallow.
        print(*checkout(os.environ, *args.directories), sep='\n')
    elif args.command == 'okilum-remote':
        sys.exit(0 if okilum_remote(args.url) else 1)
    elif args.command == 'next-build':
        build, newest = next_build(Path(f).read_text() for f in args.feeds)
        print(build, newest)
    elif args.command == 'quit':
        sys.exit(quit_gui(args.env_out, args.timeout))
    elif args.command == 'launch':
        sys.exit(launch(args.image, args.env, args.log, args.wait))
    elif args.command == 'record-build':
        record_build(args.image, args.build)


if __name__ == '__main__':
    main()
DEV_HELPER
# A lock left by a build that died (crash, closed terminal, reboot) is taken
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
lock_owned=0
relaunch_pending=0
gui_env="$work/gui-env.json"
created=()
# shellcheck disable=SC2329 # invoked by the EXIT trap
dev_exit() {
    local result=$?
    trap - EXIT
    for path in "${created[@]}"; do rm -f "$path"; done
    if ((relaunch_pending)) && [[ -e ${image:-} ]]; then
        echo 'Starting Okilum again after the failure:'
        python3 "$helper" launch --image "$image" --env "$gui_env" --log "$app_log" || true
    fi
    if ((lock_owned)); then rm -f "$root/build.lock/owner"; rmdir "$root/build.lock" 2>/dev/null || true; fi
    rm -rf "$work"
    exit "$result"
}
trap dev_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
die() { echo "$*" >&2; exit 1; }
# Cargo never deletes old dependency-graph variants, so a warm target only grows.
# Inside a build-slot run the slot owns the target and bounds it; elsewhere the
# one warm target is emptied once it passes OKILUM_DEV_TARGET_CAP_GIB.
pick_target() {
    local cap=${OKILUM_DEV_TARGET_CAP_GIB:-30} used
    [[ $cap =~ ^[0-9]+$ ]] || die 'OKILUM_DEV_TARGET_CAP_GIB must be a whole number of GiB.'
    target=${BUILD_SLOT:+${CARGO_TARGET_DIR:-}}
    [[ -z $target ]] || return 0
    target=$root/target
    [[ -d $target ]] || return 0
    used=$(du -sx --block-size=1G -- "$target" | cut -f1)
    if ((used > cap)); then
        printf 'Target %s holds %s GiB, above the %s GiB cap; starting it fresh.\n' "$target" "$used" "$cap"
        rm -rf -- "$target"
    fi
}

if [[ -f $HOME/.cargo/env ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
for command in git curl python3 cargo flock realpath sha256sum readelf gpg gpgv; do
    command -v "$command" >/dev/null || die "Missing $command; install it and rerun."
done
root=${OKILUM_DEV_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/okilum-dev}
root=${root%/}
if [[ $root != /* || $root == / || $root == "$HOME" ]]; then
    die 'OKILUM_DEV_HOME must be an absolute, dedicated directory.'
fi
for path in "$root" "$root/src" "$root/target" "$root/logs" "$root/repo"; do
    [[ ! -L $path ]] || die "Refusing symlink: $path"
done
mkdir -p "$root/logs"
acquire_lock "$root/build.lock" || exit 1
lock_owned=1
stamp=$(date +%Y%m%d-%H%M%S)
log="$root/logs/$stamp-${ref//[^A-Za-z0-9._-]/_}.log"
app_log="$root/logs/$stamp-okilum.log"
exec > >(tee -a "$log") 2>&1
# Keep the logs of the newest 20 runs (build + app log each).
find "$root/logs" -maxdepth 1 -type f -name '*.log' -printf '%T@ %p\n' | sort -rn | tail -n +41 | cut -d' ' -f2- | xargs -r rm -f --
printf 'Okilum dev build of %s · log %s\n' "$ref" "$log"

checkout_lines=$(python3 "$helper" checkout "$script_dir" "$start_dir")
{ IFS= read -r checkout; IFS= read -r checkout_source; } <<<"$checkout_lines"
if [[ -z $checkout ]]; then
    printf 'No Okilum checkout (%s): using a clone in %s\n' "$checkout_source" "$root/repo"
    checkout=$root/repo
    checkout_source="clone of ${OKILUM_DEV_REPO:-https://git.oklabs.uk/BeFeast/okilum.git}"
    [[ ! -L $checkout ]] || die "Refusing symlink: $checkout"
    if [[ ! -e $checkout ]]; then git clone --no-checkout "${OKILUM_DEV_REPO:-https://git.oklabs.uk/BeFeast/okilum.git}" "$checkout"; fi
fi
git -C "$checkout" rev-parse --git-dir >/dev/null 2>&1 || die "Not a git checkout: $checkout"
repo=$(git -C "$checkout" remote get-url origin 2>/dev/null || true)
python3 "$helper" okilum-remote "$repo" || die "Unexpected origin in $checkout: $repo"
printf 'Checkout: %s (%s), origin %s\n' "$checkout" "$checkout_source" "$repo"
src=$root/src
# Checking out a ref under a running script would rewrite the script bash is reading.
case "$script_dir/" in "$src"/*) die "Run this script from your checkout, not from the build worktree $src." ;; esac
if git check-ref-format --branch "$ref" >/dev/null 2>&1 &&
    git -C "$checkout" ls-remote --exit-code --heads origin "refs/heads/$ref" >/dev/null 2>&1; then
    git -C "$checkout" fetch --quiet origin "+refs/heads/$ref:refs/remotes/origin/$ref"
    sha=$(git -C "$checkout" rev-parse "refs/remotes/origin/$ref^{commit}")
elif [[ $ref =~ ^[0-9a-fA-F]{7,40}$ ]]; then
    git -C "$checkout" fetch --quiet origin '+refs/heads/*:refs/remotes/origin/*'
    sha=$(git -C "$checkout" rev-parse --verify --quiet "$ref^{commit}") || die "Commit $ref is not on any branch of $repo."
else
    die "No branch $ref on $repo."
fi
if [[ ! -e $src ]]; then
    git -C "$checkout" worktree prune
    git -C "$checkout" worktree add --quiet --detach "$src" "$sha"
fi
cd "$src"
[[ $(git rev-parse --show-toplevel) == "$src" ]] || die "Not the build worktree: $src"
[[ $(git rev-parse --path-format=absolute --git-common-dir) == "$(git -C "$checkout" rev-parse --path-format=absolute --git-common-dir)" ]] ||
    die "$src is not a worktree of $checkout; remove it and rerun."
# dist/ holds the previous AppImage build; anything else local is someone's work.
if [[ -n $(git status --porcelain --untracked-files=normal -- . ':(exclude)dist') ]]; then
    die "The build worktree has local changes; keep them elsewhere before rebuilding: $src"
fi
git checkout --quiet --detach "$sha"
short_sha=$(git rev-parse --short HEAD)
grep -q OKILUM_LOCAL_DEV scripts/appimage/build.sh ||
    die "$ref ($short_sha) predates local dev builds (no OKILUM_LOCAL_DEV in scripts/appimage/build.sh); merge origin/main into it first."

if [[ -n ${OKILUM_DEV_BUILD:-} ]]; then
    [[ $OKILUM_DEV_BUILD =~ ^[1-9][0-9]*$ ]] || die 'OKILUM_DEV_BUILD must be a number.'
    build=$OKILUM_DEV_BUILD
    printf 'Build %s (OKILUM_DEV_BUILD)\n' "$build"
else
    feeds=https://updates.befeast.com/okilum/linux
    curl -fsS -A curl --max-time 30 "$feeds/beta/latest.json" -o "$work/beta.json" ||
        die 'Cannot read the Linux beta feed; set OKILUM_DEV_BUILD to a number above the newest beta and rerun.'
    curl -fsS -A curl --max-time 30 "$feeds/stable/latest.json" -o "$work/stable.json" || : > "$work/stable.json"
    versions=$(python3 "$helper" next-build "$work/beta.json" "$work/stable.json")
    read -r build newest <<<"$versions"
    printf 'Build %s: one above the newest published Linux build %s, so no beta replaces it and the next one does.\n' "$build" "$newest"
fi

pick_target
printf '\nBuilding %s %s (profile %s, target %s)\n' "$ref" "$short_sha" "${OKILUM_DEV_PROFILE:-release}" "$target"
build_started=$SECONDS
if ! CARGO_TARGET_DIR="$target" OKILUM_LOCAL_DEV=1 OKILUM_BUILD_VERSION="$build" \
    OKILUM_CARGO_PROFILE="${OKILUM_DEV_PROFILE:-release}" bash scripts/appimage/build.sh; then
    echo "Build failed (full log: $log). First compiler error, or else the last lines:" >&2
    { awk '/^error/ {print; n=8} n>0 {if ($0 !~ /^error/) print; n--}' "$log" | head -25 | grep . ||
        tail -n 20 "$log"; } >&2 || true
    exit 1
fi
appimage="$src/dist/appimage/Okilum-x86_64.AppImage"
[[ -f $appimage && -x $appimage ]] || die "The build produced no AppImage at $appimage."
printf 'Built %s in %ss\n' "$appimage" "$((SECONDS - build_started))"

dir="$HOME/.local/share/okilum"
image="$dir/Okilum.AppImage"
quit_status=3
if ((restart)); then
    set +e
    python3 "$helper" quit --env-out "$gui_env" --timeout "${OKILUM_DEV_QUIT_SECONDS:-30}"
    quit_status=$?
    set -e
    if ((quit_status == 0)); then relaunch_pending=1; fi
fi
# Okilum's installer and updater take these locks (flock); an update being
# staged or applied is never overwritten.
install_image() {
    local new=$1 staged previous ilock ulock mask seconds
    if [[ -L $dir || (-e $dir && ! -d $dir) ]]; then echo "Refusing $dir: not a plain directory" >&2; return 1; fi
    mkdir -p "$dir"
    if [[ -L $image || (-e $image && ! -f $image) ]]; then echo "Refusing $image: not a regular file" >&2; return 1; fi
    if [[ -L $dir/.Okilum.AppImage.update-lock ]]; then echo "Refusing symlinked update lock in $dir" >&2; return 1; fi
    exec {ilock}<>"$dir/.install-lock"
    mask=$(umask)
    umask 077 # Okilum refuses an update lock readable by others.
    exec {ulock}<>"$dir/.Okilum.AppImage.update-lock"
    umask "$mask"
    seconds=${OKILUM_DEV_LOCK_SECONDS:-30}
    if ! flock -w "$seconds" "$ilock" || ! flock -w "$seconds" "$ulock"; then
        echo 'An Okilum install or update holds its lock; quit Okilum and rerun.' >&2
        return 1
    fi
    staged=$(mktemp "$dir/.okilum-dev-new.XXXXXX")
    created+=("$staged")
    cp "$new" "$staged"
    chmod 755 "$staged"
    sync "$staged"
    if [[ -f $image ]]; then
        previous=$(mktemp "$dir/.okilum-dev-previous.XXXXXX")
        created+=("$previous")
        cp --preserve=mode,timestamps "$image" "$previous"
        mv -fT "$previous" "$image.previous"
    fi
    mv -fT "$staged" "$image"
    exec {ilock}>&- {ulock}>&-
}
install_image "$appimage"
python3 "$helper" record-build "$image" "$build" || true
printf 'Installed %s\n' "$image"
result=0
if ((relaunch_pending)); then
    relaunch_pending=0
    python3 "$helper" launch --image "$image" --env "$gui_env" --log "$app_log" || result=1
elif ((restart && quit_status == 1)); then
    result=3
fi
printf '\nOkilum 0.1.%s (build %s) · %s %s · %ss\n' "$build" "$build" "$ref" "$sha" "$((SECONDS - started))"
if [[ -f $image.previous ]]; then
    printf 'Back to the previous build: mv -f %q %q\n' "$image.previous" "$image"
fi
case $result in
    1) echo "Installed, but Okilum did not start; its output: $app_log" >&2 ;;
    3) echo 'Installed; the running Okilum keeps the old build until you quit and reopen it.' >&2 ;;
esac
exit "$result"
