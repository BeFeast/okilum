"""Refuse an app binary built with GPUI test-support (#1043).

`cargo build --bin okilum --example …` in one invocation compiles the app with
okilum-shell's dev-dependency features (gpui `test-support`), which changes
runtime behaviour and made native checks fail falsely. Every native check
calls `require_app_binary(binary)` first: such a run is INVALID (exit 2), not
a product failure. Build the app alone: `cargo build -p okilum-shell --bin okilum`.
"""
import subprocess
import sys

INVALID = 2


def build_info(binary):
    """`okilum --build-info` as a dict, or None for a binary that predates it."""
    try:
        result = subprocess.run([str(binary), '--build-info'], capture_output=True,
                                text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    info = {}
    for line in result.stdout.splitlines():
        key, sep, value = line.partition(':')
        if sep:
            info[key.strip()] = value.strip()
    return info if 'gpui-test-support' in info else None


def require_app_binary(binary):
    info = build_info(binary)
    if info is None:
        print(f'warning: {binary} predates --build-info (#1043); '
              'cannot verify it was built without GPUI test-support', file=sys.stderr)
        return
    # The kit, platform, text, project and brain crates' test seams (#3233, #3234, #3235, #3236, #3237) leak the same way as GPUI's.
    if info.get('shell-kit-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-kit test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-platform-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-platform test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-text-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-text test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-viewers-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-viewers test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-logs-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-logs test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-sync-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-sync test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-project-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-project test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info.get('shell-brain-test-support', 'no') != 'no':
        print(f'INVALID run: {binary} was built with okilum-shell-brain test-support '
              '(app and a test or example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
    if info['gpui-test-support'] != 'no':
        print(f'INVALID run: {binary} was built with GPUI test-support '
              '(app and an example in one cargo invocation). Rebuild it alone: '
              'cargo build -p okilum-shell --bin okilum', file=sys.stderr)
        sys.exit(INVALID)
