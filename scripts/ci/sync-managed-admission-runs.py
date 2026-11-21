#!/usr/bin/env python3
"""Run every #2582/#2043/#2504/#2508 control under one unchanged 600-second deadline.

Independent crash checkpoints have separate parent processes and private state
roots. At most two run together; each remains a subreaper of its own helpers.
The owning Python process checks and reaps the complete fixture tree on exit.

A HOST_DISCOVERY test runs alone: its product path scans /proc for every
same-uid Syncthing and correctly refuses any it does not own, so a concurrent
group's live fixture runtime would fail it (#3069). On a build-slot pool slot
(OKILUM_POOL_SHARD set) the other slots of the host run as the same user, so
`alone` is not enough there: the test runs in a private PID namespace
(pid-namespace.py, needs passwordless sudo), after a positive control shows a
same-uid decoy Syncthing outside is invisible inside (#3186 review).

OKILUM_ADMISSION_PART selects the processes (default all). The pool gate runs
`crash` (both checkpoint halves), `remaining-0` and `remaining-1` (the other
parent tests, split by sorted name) on three shards, so no process waits behind
another. Each part keeps the unchanged 600 s deadline, and every part checks
the complete compiled inventory before it selects its own tests.
"""
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
pidns = runpy.run_path(str(Path(__file__).with_name('pid-namespace.py')), run_name='pidns')
CRASH_TEST = 'issue2043_crash_recovery_retains_versions_and_rejects_third_version'
TESTS = {
    'issue2582_configuration_process_drains_and_later_real_effects_are_refused',
    'issue2582_autonomous_restart_fences_dead_holder_and_other_folder_transfers',
    'issue2582_missing_root_never_stops_the_runtime_across_launches',
    'issue2582_unsafe_missing_roots_fail_closed_without_recreating_them',
    'issue2582_folder_added_after_launch_gains_evidence_without_restart',
    'issue2582_readded_folder_id_never_inherits_index_evidence',
    'issue2582_missing_root_admission_is_held_through_startup',
    'issue2582_redirected_missing_root_is_refused',
    'issue2582_root_swapped_before_start_never_syncs',
    'issue2582_launcher_killed_after_fencing_keeps_the_original_marker',
    'issue2582_root_lost_while_running_is_fenced_again_every_time',
    'issue2582_stale_fence_record_never_resumes_a_user_pause',
    'issue2582_replaced_root_never_syncs_between_polls_or_under_the_gate',
    'issue2582_linux_bootstrap_and_replaced_bindings_fail_closed',
    'issue2043_scoped_hold_blocks_real_transfer_and_recovers_across_restart',
    'issue2504_reader_writers_and_pinned_transport_share_one_restart_safe_hold',
    'issue2508_scoped_receipt_refuses_unproven_states_and_recovers_each_boundary',
    CRASH_TEST,
    'issue2043_two_managed_instances_preview_union_preservation_and_bidirectional_handoff',
    'issue3079_removed_computer_rejoins_and_a_stale_removal_cannot_remove_it_again',
    'issue3079_rejoin_through_another_computer_reuses_the_kept_copy',
    'issue3079_offline_removal_then_rejoin_through_another_computer',
    'issue3105_receiver_shares_onward_then_refreshes_and_removes_that_computer',
}
# linux_bootstrap::connect_with repeats complete host discovery
# (direct::configuration_paths) before taking lifecycle authority.
HOST_DISCOVERY = {
    'issue2582_linux_bootstrap_and_replaced_bindings_fail_closed',
}
assert HOST_DISCOVERY <= TESTS
# #3080: these fill a private tmpfs; scripts/ci/sync-disk-full.sh mounts and runs them.
DISK_FULL = {
    'issue3080_first_merge_disk_full_keeps_every_version_and_recovers',
    'issue3080_transfer_disk_full_needs_attention_keeps_files_and_resumes',
}
CHECKPOINTS = (
    'destination-prepared', 'source-staged', 'source-partial-blob', 'source-prepared',
    'source-frozen', 'source-captured', 'source-note-captured', 'source-published',
    'destination-published', 'source-complete', 'destination-released',
    'handoff-folder-created', 'third-version',
)


def select(part):
    """The processes of one admission part, in start order."""
    remaining = sorted(TESTS - {CRASH_TEST})
    crash = [('crash-even', CHECKPOINTS[::2], None), ('crash-odd', CHECKPOINTS[1::2], None)]
    if part == 'all':
        return crash + [('remaining', (), None)]
    if part == 'crash':
        return crash
    if part in ('remaining-0', 'remaining-1'):
        index = int(part[-1])
        return [(part, (), set(remaining[index::2]))]
    raise SystemExit(f'unknown OKILUM_ADMISSION_PART: {part}')


def groups_of(processes):
    """(name, crash checkpoints, exact tests, runs alone); HOST_DISCOVERY runs alone first (#3069)."""
    alone, groups = [], []
    for name, checkpoints, tests in processes:
        if checkpoints:
            groups.append((name, checkpoints, [CRASH_TEST], False))
            continue
        discovery = HOST_DISCOVERY if tests is None else tests & HOST_DISCOVERY
        if discovery:
            alone.append(('host-discovery', (), sorted(discovery), True))
        rest = None if tests is None else sorted(tests - HOST_DISCOVERY)
        assert rest is None or rest, name
        groups.append((name, (), rest, False))
    return alone + groups


def main():
    lifecycle['subreaper']()
    work = Path(tempfile.mkdtemp(prefix='sync-managed-admission-', dir=os.environ['TMPDIR']))
    executable = str(Path(sys.argv[1]).resolve())
    # Discover the exact compiled parent inventory before selecting any shard.
    listing = subprocess.run([executable, '--list'], check=True, text=True, stdout=subprocess.PIPE).stdout
    discovered = {line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')}
    assert discovered == TESTS | DISK_FULL | {'issue2582_child'}, discovered
    # The #3079 rejoin controls run inside 'remaining': a further parallel
    # process of multi-runtime fixtures made port reuse hit other tests.
    groups = groups_of(select(os.environ.get('OKILUM_ADMISSION_PART') or 'all'))
    print(f'Managed admission fixture evidence: {work}', flush=True)
    active, all_runs = [], []
    deadline = time.monotonic() + 600

    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)

    # The same scope as direct::configuration_paths: this uid, exe named syncthing.
    foreign_syncthing = pidns['same_uid_syncthing']
    isolate = bool(os.environ.get('OKILUM_POOL_SHARD'))
    tokens = {}

    def start(name, checkpoints, selected, alone):
        if alone:
            assert not active, (name, [run[0] for run in active])
            if isolate:
                # Sibling slots may legitimately run Syncthing fixtures; the
                # namespace hides them and its init re-checks inside.
                pidns['probe'](work)
            else:
                assert not foreign_syncthing(), (name, 'same-uid Syncthing before host discovery', foreign_syncthing())
        root = work / name
        root.mkdir(mode=0o700)
        (root / 'state').mkdir(mode=0o700)
        (root / 'tmp').mkdir()
        env = dict(os.environ, TMPDIR=str(root / 'tmp'), XDG_STATE_HOME=str(root / 'state'), RUST_BACKTRACE='1')
        # Never inherit a caller's narrowed selection into the complete gate.
        env.pop('OKILUM_FIRST_SYNC_CRASH_CASE', None)
        command = [executable, '--include-ignored', '--nocapture', '--test-threads=1']
        if selected:
            command += [*selected, '--exact']
        else:
            command += ['--skip', 'issue2582_child', '--skip', CRASH_TEST, '--skip', 'issue3080_']
            for test in sorted(HOST_DISCOVERY):
                command += ['--skip', test]
        if checkpoints:
            env['OKILUM_FIRST_SYNC_CRASH_CASE'] = ','.join(checkpoints)
        log = root / 'acceptance.log'
        if alone and isolate:
            command, _, tokens[name] = pidns['command'](command, env, root)
        with log.open('w') as output:
            process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL, stdout=output,
                                       stderr=subprocess.STDOUT, preexec_fn=lifecycle['parent_death_signal']())
        run = (name, checkpoints, process, log, selected, alone)
        active.append(run)
        all_runs.append(run)

    try:
        while groups or active:
            # A group that runs alone waits for an empty slot set and blocks the rest.
            while groups and len(active) < 2 and not any(run[5] for run in active) \
                    and not (active and groups[0][3]):
                start(*groups.pop(0))
            assert time.monotonic() < deadline, 'managed admission exceeded the unchanged 600s deadline'
            for run in active[:]:
                name, checkpoints, process, log, selected, _ = run
                if process.poll() is None:
                    continue
                process.wait()
                output = log.read_text()
                print(f'=== Managed admission {name} ===\n{output}', flush=True)
                expected = set(selected) if selected else TESTS - {CRASH_TEST} - HOST_DISCOVERY
                for test in expected:
                    assert f'test {test} ...' in output, (name, test, output)
                assert process.returncode == 0 and f'test result: ok. {len(expected)} passed; 0 failed;' in output, (name, process.returncode, output)
                observed = [line.split('checkpoint=', 1)[1].split()[0]
                            for line in output.splitlines() if line.startswith('FIRST_SYNC_CRASH_RECOVERY_PASS ')]
                assert observed == list(checkpoints), (name, observed, checkpoints)
                assert not lifecycle['check'](log.parent), f'surviving {name} fixture process'
                active.remove(run)
            time.sleep(.05)
        assert not lifecycle['check'](work), 'surviving fixture process'
        lifecycle['reap']()
        selected = set()
        checkpoints_run = 0
        for _, checkpoints, _, _, tests, _ in all_runs:
            selected |= set(tests) if tests else TESTS - {CRASH_TEST} - HOST_DISCOVERY
            checkpoints_run += len(checkpoints)
        print(f'MANAGED_ADMISSION_PASS: part={os.environ.get("OKILUM_ADMISSION_PART") or "all"} '
              f'{len(selected)} selected tests and {checkpoints_run} crash checkpoints passed; '
              'fixture processes=0; children reaped', flush=True)
        return 0
    finally:
        for name, _, process, log, _, _ in all_runs:
            if process.poll() is None:
                if name in tokens:
                    # sudo is root-owned; ending the namespace init ends the run.
                    pidns['kill_marked'](tokens[name])
                try:
                    process.kill()
                except PermissionError:
                    pass
            process.wait(timeout=60)
            if any(run[0] == name for run in active):
                print(f'=== Incomplete managed admission {name} ===\n{log.read_text()}', flush=True)
        lifecycle['check'](work, cleanup=True)
        lifecycle['reap']()


if __name__ == '__main__':
    raise SystemExit(main())
