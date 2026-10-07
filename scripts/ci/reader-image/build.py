#!/usr/bin/env python3
"""Build an isolated Reader CI image candidate; never changes the active CI image."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
import uuid

REPOSITORY = "registry.oklabs.uk/tessera-ci-reader"
RECIPE = Path(__file__).resolve().parent
ROOT = RECIPE.parents[2]


def inputs(root, recipe):
    toolchain_bytes = (root / "rust-toolchain.toml").read_bytes()
    config = tomllib.loads(toolchain_bytes.decode())["toolchain"]
    version = config.get("channel", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("rust-toolchain.toml must pin an exact Rust release")
    if config.get("profile") != "minimal":
        raise ValueError("image recipe supports only the minimal toolchain profile")
    if set(config.get("components", [])) != {"clippy", "rustfmt"} or config.get("targets"):
        raise ValueError("toolchain components/targets changed: review the image recipe")
    base = (recipe / "base-image.txt").read_text().strip()
    if not re.fullmatch(r"ghcr\.io/catthehacker/ubuntu@sha256:[0-9a-f]{64}", base):
        raise ValueError("base image must be pinned by digest")
    packages = (recipe / "packages.txt").read_text().splitlines()
    if not packages or any(not re.fullmatch(r"[a-z0-9][a-z0-9+.-]*", p) for p in packages):
        raise ValueError("invalid apt dependency manifest")
    return version, base, toolchain_bytes


def prepare_context(root, recipe, directory):
    version, base, toolchain_bytes = inputs(root, recipe)
    # Explicit allowlist: no source, target, .git, vault, cache or credentials.
    for name in ("Dockerfile", "packages.txt"):
        shutil.copyfile(recipe / name, directory / name)
    (directory / "rust-toolchain.toml").write_bytes(toolchain_bytes)
    return version, base


def run(*args):
    subprocess.run(args, check=True)


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--publish", action="store_true", help="push a unique candidate tag to the LAN registry")
    parser.add_argument("--output", type=Path, default=ROOT / "target/ci-reader-image")
    args = parser.parse_args()
    artifact = args.output.resolve()
    artifact.mkdir(parents=True, exist_ok=True)
    # A failed attempt must not leave a previous successful receipt at this path.
    (artifact / "receipt.json").unlink(missing_ok=True)
    # All scratch is scoped to the caller-selected target/cache directory, never /tmp.
    version, base, toolchain_bytes = inputs(ROOT, RECIPE)
    revision = output("git", "-C", str(ROOT), "rev-parse", "HEAD")
    tracked = ["rust-toolchain.toml", "scripts/ci/reader-image"]
    if output("git", "-C", str(ROOT), "status", "--porcelain", "--", *tracked):
        raise ValueError("commit image inputs before building provenance")
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    tag = f"{REPOSITORY}:candidate-{revision[:12]}-{stamp}-{uuid.uuid4().hex[:12]}"
    receipt = {"source_revision": revision, "rust": version, "base": base, "tag": tag,
               "toolchain_sha256": hashlib.sha256(toolchain_bytes).hexdigest(), "published": False}
    with tempfile.TemporaryDirectory(prefix="context-", dir=artifact) as scratch:
        context = Path(scratch)
        prepare_context(ROOT, RECIPE, context)
        run("docker", "build", "--pull", "--platform=linux/amd64", "--tag", tag,
            "--build-arg", f"BASE_IMAGE={base}", "--build-arg", f"RUST_VERSION={version}",
            "--build-arg", f"SOURCE_REVISION={revision}", str(context))
    # Smoke only: checkout/cache actions and the full Reader gate need paired canaries.
    actual = output("docker", "run", "--rm", "--network=none", tag, "rustc", "--version")
    if actual.split()[1] != version:
        raise ValueError(f"built toolchain mismatch: {actual}")
    run("docker", "run", "--rm", "--network=none", tag, "bash", "-ec",
        'test "$HOME" = /root; cargo clippy -V; rustfmt -V; node --version; '
        'python3 --version; git --version; mold --version; '
        'test ! -e /root/.cargo/credentials.toml; test ! -e /root/.docker/config.json')
    receipt["image_id"] = output("docker", "image", "inspect", tag, "--format", "{{.Id}}")
    if args.publish:
        # Uses pre-provisioned Docker auth if required; never accepts/logs a token.
        run("docker", "push", tag)
        digests = json.loads(output("docker", "image", "inspect", tag, "--format", "{{json .RepoDigests}}"))
        candidates = [d for d in digests if d.startswith(REPOSITORY + "@sha256:")]
        if len(candidates) != 1:
            raise ValueError("cannot identify the pushed immutable digest")
        receipt.update(published=True, digest=candidates[0])
    (artifact / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
