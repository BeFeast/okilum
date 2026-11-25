//! Unit tests of this binary never touch the real per-user writer state (#3271).

#[test]
fn shell_tests_use_a_private_writer_state_and_temp_directory() {
    assert!(okilum_write_authority::test_isolation::active());
    let root = okilum_write_authority::test_isolation::root().unwrap();
    assert!(okilum_write_authority::account_directory()
        .unwrap()
        .starts_with(&root));
    assert!(std::env::temp_dir().starts_with(&root));
}
