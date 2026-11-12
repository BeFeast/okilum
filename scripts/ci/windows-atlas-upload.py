#!/usr/bin/env python3
"""Run real WARP uploads and compiled runtime controls, then restore pinned bytes."""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "vendor/gpui-pre-windows-0.3.3/src/directx_atlas.rs"
OUT = ROOT / "target/windows-atlas-upload-controls"


def build(label):
    args = ["cargo", "test", "--locked", "-p", "gpui-pre-windows", "--lib", "--no-run", "--message-format=json-render-diagnostics"]
    result = subprocess.run(args, cwd=ROOT, text=True, stdout=subprocess.PIPE)
    (OUT / (label + ".jsonl")).write_text(result.stdout, encoding="utf-8")
    assert result.returncode == 0, "a compile failure is never a fired negative control"
    rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    binary = next(row["executable"] for row in reversed(rows) if row.get("reason") == "compiler-artifact" and row.get("profile", {}).get("test") and row.get("executable"))
    destination = OUT / (label + ".exe")
    shutil.copy2(binary, destination)
    return destination


def run(binary, label, test="directx_atlas::tests", *, control=None, failure=None):
    env = dict(os.environ)
    if control:
        env["OKILUM_ATLAS_UPLOAD_CONTROL"] = control
    else:
        env.pop("OKILUM_ATLAS_UPLOAD_CONTROL", None)
    result = subprocess.run([str(binary), test, "--nocapture", "--test-threads=1"], cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (OUT / (label + ".log")).write_text(result.stdout, encoding="utf-8")
    print(result.stdout, flush=True)
    if failure:
        assert result.returncode != 0 and "test result: FAILED" in result.stdout and failure in result.stdout, label
    else:
        assert result.returncode == 0 and "test result: ok. 3 passed" in result.stdout, label


def instrument(original):
    source = original.decode("utf-8")
    anchor = "        let row_bytes = bounds.size.width.to_bytes(self.bytes_per_pixel as u8) as usize;"
    assert source.count(anchor) == 1
    legacy = '''        #[cfg(test)]
    if std::env::var("OKILUM_ATLAS_UPLOAD_CONTROL").ok().as_deref() == Some("legacy") {
        unsafe { device_context.UpdateSubresource(&self.texture,0,Some(&windows::Win32::Graphics::Direct3D11::D3D11_BOX {
            left:bounds.left().0 as u32,top:bounds.top().0 as u32,front:0,
            right:bounds.right().0 as u32,bottom:bounds.bottom().0 as u32,back:1,
        }),bytes.as_ptr() as _,bounds.size.width.to_bytes(self.bytes_per_pixel as u8),0); }
        return Ok(());
    }
'''
    source = source.replace(anchor, legacy + anchor)
    anchor = "        anyhow::ensure!("
    assert source.count(anchor) == 1
    source = source.replace(anchor, '''        #[cfg(test)]
    if std::env::var("OKILUM_ATLAS_UPLOAD_CONTROL").ok().as_deref() == Some("invalid-success") && bytes.len() < expected {return Ok(());}
''' + anchor)
    anchor = "                lock.discard(tile);"
    assert source.count(anchor) == 1
    source = source.replace(anchor, '''                if std::env::var("OKILUM_ATLAS_UPLOAD_CONTROL").ok().as_deref() != Some("leak") { lock.discard(tile); }''')
    # Limit the destination mutant to the upload; leave the readback oracle intact.
    start = source.index("    fn upload(")
    end = source.index("    fn decrement_ref_count", start)
    body = source[start:end]
    anchor = "row * mapped.RowPitch as usize"
    assert body.count(anchor) == 1
    body = body.replace(anchor, 'row * if std::env::var("OKILUM_ATLAS_UPLOAD_CONTROL").ok().as_deref()==Some("packed-pitch") {row_bytes} else {mapped.RowPitch as usize}')
    anchor = "                bounds.left().0 as u32,"
    assert body.count(anchor) == 1
    body = body.replace(anchor, '''                if std::env::var("OKILUM_ATLAS_UPLOAD_CONTROL").ok().as_deref()==Some("wrong-origin") {0} else {bounds.left().0 as u32},''')
    source = source[:start] + body + source[end:]
    return source


def main():
    assert os.name == "nt", "the atlas test requires the real Windows DirectX backend"
    OUT.mkdir(parents=True, exist_ok=True)
    original = SOURCE.read_bytes()
    try:
        positive = build("positive")
        run(positive, "positive")
        SOURCE.write_text(instrument(original), encoding="utf-8")
        negative = build("compiled-controls")
        run(negative, "compiled-positive")
        readback = "atlas_upload_copies_padded_rows_at_nonzero_offsets_and_keeps_existing_pixels"
        failure = "atlas_upload_failure_releases_allocation_and_does_not_cache_missing_pixels"
        for control, test, message in [
            ("legacy", readback, "real upload must use staging/copy"),
            ("invalid-success", failure, "truncated upload must not claim success"),
            ("leak", failure, "failed upload must release its tile"),
            ("packed-pitch", readback, "assertion `left == right` failed"),
            ("wrong-origin", readback, "assertion `left == right` failed"),
        ]:
            run(negative, control, test, control=control, failure=message)
    finally:
        SOURCE.write_bytes(original)
    subprocess.run(["python", "scripts/gpui-pre-windows-setup.py", "--verify"], cwd=ROOT, check=True)
    # Leave the normal test artifact in Cargo's cache, not the runtime mutants.
    run(build("restored-positive"), "restored-positive")
    print("WINDOWS_ATLAS_REAL_WARP_AND_FIVE_COMPILED_CONTROLS_PASS", flush=True)


if __name__ == "__main__":
    main()
