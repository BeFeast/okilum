#!/usr/bin/env python3
"""Run staged extraction controls and prove the corresponding unsafe SDK mutations fail."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-http", "--features", "updates", "--test", "zip_compat"]


def require_failure(test):
    result = subprocess.run(COMMAND + [test, "--", "--exact", "--nocapture"], cwd=ROOT,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    if result.returncode != 101 or f"{test} ... FAILED" not in result.stdout or "test result: FAILED" not in result.stdout:
        raise RuntimeError("Unsafe SDK mutation did not fail the executed regression: " + test)


def main():
    subprocess.run(COMMAND + ["--", "--nocapture"], cwd=ROOT, check=True)
    source = ROOT / "vendor/velopack-1.2.161/src/bundle.rs"
    original = source.read_bytes()
    staging = b"let mut outfile = tempfile::NamedTempFile::new_in(parent)?;"
    publication = b"        outfile.as_file().sync_all()?;\n        publish_staged_file(outfile, path)?;\n"
    selection = b"zip::SUPPORTED_COMPRESSION_METHODS.contains(&file.compression())\n                    && "
    if any(original.count(marker) != 1 for marker in (staging, publication, selection)):
        raise RuntimeError("Pinned extraction patch is missing or ambiguous")
    try:
        source.write_bytes(original.replace(staging, b"let mut outfile = File::create(path)?;").replace(publication, b""))
        require_failure("failed_crc_after_partial_copy_keeps_original_helper_identical")
        require_failure("failed_size_check_keeps_original_helper_identical")
        source.write_bytes(original.replace(selection, b""))
        require_failure("unsupported_first_helper_is_skipped_and_supported_helper_replaces_original")
        stub = b"            self.extract_zip_idx_to_path(i, &dest_path)?;"
        assert original.count(stub) == 1
        source.write_bytes(original.replace(stub, b"            let _ = self.extract_zip_idx_to_path(i, &dest_path);"))
        require_failure("failed_stub_extraction_returns_error_and_keeps_original")
        if __import__("os").name == "nt":
            source.write_bytes(original.replace(b"publish_staged_file(outfile, path)?;", b"outfile.persist(path).map_err(|e| e.error)?;"))
            require_failure("windows::replacement_preserves_explicit_execute_ace_and_nondefault_attributes")
    finally:
        source.write_bytes(original)
    subprocess.run(["python3" if __import__("os").name != "nt" else "python", "scripts/velopack-setup.py", "--verify"], cwd=ROOT, check=True)
    if __import__("os").name == "nt":
        manager = ROOT / "vendor/velopack-1.2.161/src/manager.rs"
        manager_original = manager.read_bytes()
        before_publish = b"        #[cfg(windows)]\n        self.prepare_update_helper(&partial_file)?;\n"
        cached_retry = b"            #[cfg(windows)]\n            self.prepare_update_helper(&final_target_file)?;\n"
        pending_guard = b"                if !self.is_update_helper_prepared(&asset).unwrap_or(false) { return None; }"
        assert all(manager_original.count(marker) == 1 for marker in (before_publish, cached_retry, pending_guard))
        try:
            late = manager_original.replace(before_publish, b"").replace(b"        fs::rename(&partial_file, &final_target_file)?;", b"        fs::rename(&partial_file, &final_target_file)?;\n        self.prepare_update_helper(&final_target_file)?;")
            manager.write_bytes(late)
            require_failure("windows::helper_failure_never_publishes_pending_package_and_retry_prepares_helper")
            manager.write_bytes(manager_original.replace(cached_retry, b""))
            require_failure("windows::cached_package_with_stale_or_corrupted_helper_is_not_pending_and_retry_repairs_it")
            manager.write_bytes(manager_original.replace(pending_guard, b""))
            require_failure("windows::cached_package_with_stale_or_corrupted_helper_is_not_pending_and_retry_repairs_it")
        finally:
            manager.write_bytes(manager_original)
    subprocess.run(["python3" if __import__("os").name != "nt" else "python", "scripts/velopack-setup.py", "--verify"], cwd=ROOT, check=True)
    subprocess.run(COMMAND + ["--", "--nocapture"], cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
