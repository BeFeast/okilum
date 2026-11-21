#!/usr/bin/env python3
"""Compiled native controls: volatile-device, migration, UUID-less (#3202) and mount-alias (#3204) regressions."""
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
    alias_test = "identity_macos_tests::identity_3204_mount_alias_resolves_to_the_held_scope"
    child_test = "identity_macos_tests::identity_3204_child_first_then_parent_through_alias_keeps_one_scope"
    gone_test = "identity_macos_tests::identity_3204_held_child_fences_parent_after_its_mount_is_gone"
    stale_test = "identity_macos_tests::identity_3204_stale_binding_is_retired_through_another_mount"
    for test, label in [(alias_test, "alias"), (child_test, "child-first"),
                        (gone_test, "mount-gone"), (stale_test, "stale")]:
        run(test, f"{label}-positive")
    # #3204: each control removes one rule; its regression must then fail.
    controls = [
        (native, original_native, "&& (self.1, self.2) == (other.1, other.2)", "&& false",
         alias_test, "no-alias-match-negative"),
        (entry, original_entry, "Some(encloses) => encloses,", "Some(_) => false,",
         child_test, "no-ancestry-enclosure-negative"),
        (entry, original_entry, "Some(encloses) => encloses,", "Some(_) => false,",
         gone_test, "no-ancestry-after-unmount-negative"),
        (entry, original_entry, "        self.alias_enclosing()\n", "        Ok(None)\n",
         child_test, "no-alias-routing-negative"),
        (entry, original_entry, "a.retire_unmounted()?;", "let _ = &a;",
         stale_test, "no-retire-negative"),
        (entry, original_entry, "Self::set_aside_retired(&directory)?;", "",
         stale_test, "no-set-aside-negative"),
    ]
    for path, original, old, replacement, test, label in controls:
        try:
            assert original.count(old) == 1, old
            path.write_text(original.replace(old, replacement))
            run(test, label, negative=True)
        finally:
            path.write_text(original)
    run(device_test, "native-restored")
    run(legacy_test, "migration-restored")
    run(no_uuid_test, "no-uuid-restored")
    for test, label in [(alias_test, "alias"), (child_test, "child-first"),
                        (gone_test, "mount-gone"), (stale_test, "stale")]:
        run(test, f"{label}-restored")
    assert native.read_text() == original_native and entry.read_text() == original_entry


if __name__ == "__main__":
    main()
