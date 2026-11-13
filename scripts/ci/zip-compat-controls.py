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
    publication = b"        outfile.as_file().sync_all()?;\n        outfile.persist(path).map_err(|e| e.error)?;\n"
    selection = b"zip::SUPPORTED_COMPRESSION_METHODS.contains(&file.compression())\n                    && "
    if any(original.count(marker) != 1 for marker in (staging, publication, selection)):
        raise RuntimeError("Pinned extraction patch is missing or ambiguous")
    try:
        source.write_bytes(original.replace(staging, b"let mut outfile = File::create(path)?;").replace(publication, b""))
        require_failure("failed_crc_after_partial_copy_keeps_original_helper_identical")
        require_failure("failed_size_check_keeps_original_helper_identical")
        source.write_bytes(original.replace(selection, b""))
        require_failure("unsupported_first_helper_is_skipped_and_supported_helper_replaces_original")
    finally:
        source.write_bytes(original)
    subprocess.run(["python3" if __import__("os").name != "nt" else "python", "scripts/velopack-setup.py", "--verify"], cwd=ROOT, check=True)
    subprocess.run(COMMAND + ["--", "--nocapture"], cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
