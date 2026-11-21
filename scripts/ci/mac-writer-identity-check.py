#!/usr/bin/env python3
"""Compiled native controls: volatile-device and missing-migration regressions."""
import pathlib
import subprocess

QA = pathlib.Path("qa/writer-identity-2692")


def run(test, label, *, negative=False):
    result = subprocess.run(
        ["cargo", "test", "--locked", "-p", "okilum-write-authority", "--lib",
         test, "--", "--exact", "--nocapture", "--test-threads=1"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
    )
    (QA / f"{label}.log").write_text(result.stdout)
    print(result.stdout, flush=True)
    assert "running 1 test" in result.stdout, "control was not executed"
    if negative:
        assert result.returncode != 0 and "1 failed" in result.stdout, "old behavior did not fail at runtime"
    else:
        assert result.returncode == 0 and "1 passed; 0 failed" in result.stdout


def main():
    QA.mkdir(parents=True, exist_ok=True)
    native = pathlib.Path("crates/okilum-write-authority/src/identity_macos.rs")
    entry = pathlib.Path("crates/okilum-write-authority/src/lib.rs")
    original_native, original_entry = native.read_text(), entry.read_text()
    device_test = "identity_macos_tests::identity_2692_native_uuid_and_device_renumbering"
    legacy_test = "identity_macos_tests::identity_2692_legacy_binding_migrates_without_changing_state"
    run(device_test, "native-positive")
    run(legacy_test, "migration-positive")
    try:
        old = "(a, self.1, self.2) == (b, other.1, other.2)"
        assert original_native.count(old) == 1
        native.write_text(original_native.replace(old,
            "(a, self.0, self.1, self.2) == (b, other.0, other.1, other.2)"))
        run(device_test, "volatile-device-negative", negative=True)
    finally:
        native.write_text(original_native)
    try:
        call = "Self::migrate_legacy_bindings(&account, &root)?;"
        assert original_entry.count(call) == 1
        entry.write_text(original_entry.replace(call, "let _ = (&account, &root);"))
        run(legacy_test, "no-migration-negative", negative=True)
    finally:
        entry.write_text(original_entry)
    no_uuid_test = "identity_macos_tests::identity_3202_volume_without_uuid_admits_writers_across_remounts"
    run(no_uuid_test, "no-uuid-positive")
    try:
        # #3202: before the fix, a volume without ATTR_VOL_UUID (NFS) refused every edit.
        missing = "Some(libc::EINVAL | libc::ENOTSUP | libc::ENOATTR)"
        assert original_native.count(missing) == 1
        native.write_text(original_native.replace(missing, "Some(i32::MIN)"))
        run(no_uuid_test, "no-uuid-refused-negative", negative=True)
    finally:
        native.write_text(original_native)
    alias_test = "identity_macos_tests::identity_3202_mount_source_alias_cannot_bypass_a_hold"
    run(alias_test, "mount-alias-positive")
    try:
        # #3202 review: without alias discovery, a second mount spelling of the
        # same export minted a second authority and bypassed the hold.
        alias = "if stored.mount_alias_candidate(identity) {"
        assert original_entry.count(alias) == 1
        entry.write_text(original_entry.replace(alias, "if false && stored.mount_alias_candidate(identity) {"))
        run(alias_test, "mount-alias-bypass-negative", negative=True)
    finally:
        entry.write_text(original_entry)
    enclosed_test = "identity_macos_tests::identity_3202_held_child_fences_its_parent_through_another_mount"
    run(enclosed_test, "mount-alias-child-positive")
    try:
        # #3202 review: a parent opened through a second mount spelling missed a
        # held child registered through the first and minted an overlapping authority.
        guard = "|| !physically_encloses(&ancestor_identities[0], &b.root),"
        assert original_entry.count(guard) == 1
        entry.write_text(original_entry.replace(guard, "|| true,"))
        run(enclosed_test, "mount-alias-child-bypass-negative", negative=True)
    finally:
        entry.write_text(original_entry)
    run(device_test, "native-restored")
    run(legacy_test, "migration-restored")
    run(no_uuid_test, "no-uuid-restored")
    run(alias_test, "mount-alias-restored")
    run(enclosed_test, "mount-alias-child-restored")
    assert native.read_text() == original_native and entry.read_text() == original_entry


if __name__ == "__main__":
    main()
