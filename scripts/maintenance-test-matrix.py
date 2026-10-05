#!/usr/bin/env python3
"""Fail-closed maintenance posture runner; main keeps full workspace tests."""
import argparse
import importlib.util
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts"


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPT / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def posture(root=ROOT):
    text = (root / "crates/tessera-brain/src/maestro_links.rs").read_text()
    matches = re.findall(r'^pub const NEW_LINKS_ENABLED: bool = (true|false);$', text, re.MULTILINE)
    if len(matches) != 1:
        raise ValueError("Unknown maintenance capability declaration; refuse to select tests")
    return matches[0] == "true"


def exclusions():
    names = json.loads((SCRIPT / "maintenance-feature-tests.json").read_text())["tests"]
    if len(names) != 15 or len(set(names)) != 15:
        raise ValueError("Maintenance classification must contain the exact 15 distinct tests")
    return names


def test_command(enabled, listed=()):
    command = ["cargo", "test", "--workspace"]
    if not enabled:
        names = exclusions()
        missing = set(names) - set(listed)
        if missing:
            raise ValueError("Classified maintenance tests absent from actual test inventory: " + repr(sorted(missing)))
        unexpectedly_matched = [test for test in listed if test not in names and any(name in test for name in names)]
        if unexpectedly_matched:
            raise ValueError("A classified skip would hide another test: " + repr(unexpectedly_matched))
        command += ["--"]
        for name in names:
            command += ["--skip", name]
    return command


def artifact(entry, root):
    required = {"binary", "sha256", "source", "freeze", "freeze_sha256", "feature_graph"}
    if not isinstance(entry, dict) or not required <= entry.keys():
        raise ValueError("Missing actual frozen artifact provenance")
    base = module("matrix_base", "verify-maestro-maintenance.py")
    binary = (root / entry["binary"]).resolve()
    freeze = (root / entry["freeze"]).resolve()
    if not re.fullmatch(r'[0-9a-f]{40}', entry["source"]):
        raise ValueError("Source must be an exact commit")
    if base.digest(binary) != entry["sha256"] or base.digest(freeze) != entry["freeze_sha256"]:
        raise ValueError("Artifact or freeze receipt hash changed")
    receipt = json.loads(freeze.read_text())
    source = receipt.get("commit", receipt.get("source_commit"))
    hashes = receipt.get("files", {})
    actual = hashes.get("tessera-cored", receipt.get("backend_sha256", receipt.get("sha256")))
    if source != entry["source"] or actual != entry["sha256"]:
        raise ValueError("Artifact identity does not match its frozen provenance")
    if entry["feature_graph"] == "gui+cored":
        if not {"tessera", "tessera-cored"} <= receipt.get("compiler_artifacts", {}).keys():
            raise ValueError("Missing actual full GUI+cored compiler artifact provenance")
        gui_sha = hashes.get("tessera")
        if not gui_sha or not re.fullmatch(r'[0-9a-f]{64}', gui_sha):
            raise ValueError("Full GUI+cored graph requires the companion GUI artifact receipt")
        if base.digest(binary.with_name("tessera")) != gui_sha:
            raise ValueError("Companion GUI artifact differs from the release freeze")
    elif entry["feature_graph"] != "historical-cored-only":
        raise ValueError("Unknown artifact feature graph")
    return binary


def load_manifest(path):
    manifest = json.loads(path.read_text())
    if manifest.get("schema") != "tessera-maintenance-matrix-input/v1":
        raise ValueError("Unsupported matrix manifest")
    artifacts = {name: artifact(manifest[name], path.parent) for name in ["enabled", "maintenance", "old5b"]}
    if any(manifest[x]["feature_graph"] != "gui+cored" for x in ["enabled", "maintenance"]):
        raise ValueError("Supported enabled and maintenance releases must use the full GUI+cored graph")
    packet = module("matrix_packets", "verify-reviewed-packet-maintenance.py")
    if manifest["old5b"]["sha256"] != packet.OLD5B_SHA256:
        raise ValueError("Specific old5b negative control is required")
    return manifest, artifacts


def matrix(manifest, artifacts):
    maintenance = module("matrix_maintenance", "verify-maestro-maintenance.py")
    dispositions = module("matrix_dispositions", "verify-maestro-dispositions.py")
    packets = module("matrix_packets", "verify-reviewed-packet-maintenance.py")
    enabled, fallback, old5b = [artifacts[x] for x in ["enabled", "maintenance", "old5b"]]
    results = {"posture": maintenance.verify(enabled, fallback), "dispositions": dispositions.verify(enabled, fallback)}
    recovery = module("matrix_recovery", "verify-maintenance-source-recovery.py")
    results["source_recovery"] = recovery.verify(enabled, fallback)
    results["packets"] = [packets.verify(enabled, fallback), packets.verify(fallback, enabled, citation_provider=enabled), packets.verify(enabled, old5b, negative=True)]
    return {"schema": "tessera-maintenance-matrix-result/v1", "passed": True, "artifact_inputs": manifest, "checks": results, "historical_ci312": "failed, unchanged"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=pathlib.Path, help="Required manifest for maintenance tests or --matrix-only")
    parser.add_argument("--matrix-only", action="store_true", help="Run actual binary gates; never implies source unit tests ran")
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    enabled = posture()
    if args.matrix_only or not enabled:
        if args.artifacts is None:
            parser.error("Maintenance cannot pass on exclusions alone: --artifacts is required")
        manifest, artifacts = load_manifest(args.artifacts.resolve())
        if not args.matrix_only:
            current = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
            if manifest["maintenance"]["source"] != current:
                parser.error("Maintenance artifact must be frozen from this exact source commit")
    if not args.matrix_only:
        listed = ()
        if not enabled:
            # Enumerate actual Cargo test names; an unrelated/new failure stays fatal.
            output = subprocess.check_output(["cargo", "test", "--workspace", "--", "--list"], cwd=ROOT, text=True)
            listed = [line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")]
        subprocess.run(test_command(enabled, listed), cwd=ROOT, check=True)
    if args.matrix_only or not enabled:
        result = matrix(manifest, artifacts)
        result["source_unit_tests_ran"] = not args.matrix_only
        if args.output:
            args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
