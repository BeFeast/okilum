#!/usr/bin/env python3
"""Manual exact-PR native packaging verification. Artifact only; no publisher."""
import hashlib
from datetime import datetime, time
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import urllib.request
from zoneinfo import ZoneInfo

REPO = "BeFeast/okilum"
REQUIRED = ("ci / shared (pull_request)", "ci / macos (pull_request)",
            "ci / check (pull_request)")
RUNTIME = Path("Contents/MacOS/syncthing")
PROVENANCE = Path("Contents/Resources/Syncthing/provenance.json")


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def command(*args, cwd=None, env=None, timeout=60):
    return subprocess.run(args, cwd=cwd, env=env, check=True, capture_output=True,
                          text=True, timeout=timeout)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def admission(env, api):
    # Validate before checkout, code execution or signing. No automatic events.
    require(env.get("VERIFY_ONLY") == "true", "verify_only=true required")
    require(env.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
            and env.get("GITHUB_REPOSITORY") == REPO
            and env.get("GITHUB_REF") == "refs/heads/main", "trusted main manual dispatch required")
    require(env.get("GITHUB_RUN_ATTEMPT") == "1", "retry prohibited")
    source, number = env.get("VERIFY_SOURCE", ""), env.get("VERIFY_PR", "")
    require(re.fullmatch(r"[0-9a-f]{40}", source), "full source SHA required")
    require(re.fullmatch(r"[1-9][0-9]*", number), "PR number required")
    pr = api("/pulls/" + number)
    require(pr["state"] == "open" and not pr["draft"] and not pr["merged"]
            and pr["base"]["ref"] == "main"
            and pr["base"]["repo"]["full_name"] == REPO
            and pr["head"]["repo"]["full_name"] == REPO
            and pr["head"]["sha"] == source, "current trusted open PR head required")
    status = api("/commits/" + source + "/status")
    require(status["sha"] == source, "status source mismatch")
    for context in REQUIRED:
        matching = [s for s in status["statuses"] if s["context"] == context]
        require(len(matching) == 1 and matching[0]["status"] == "success",
                "required exact-head check missing or not green: " + context)
    return source


def api_get(path):
    request = urllib.request.Request("https://git.oklabs.uk/api/v1/repos/" + REPO + path,
                                     headers={"User-Agent": "OkilumNativeVerify/1"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def verify_artifact(output, source):
    app = output / "Okilum.app"
    plist = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    require(plist["OkilumSourceCommit"] == source, "app source mismatch")
    notarization = json.loads((output / "notarization.json").read_text())
    require(notarization["status"] == "Accepted", "notarization not Accepted")
    command("codesign", "--verify", "--deep", "--strict", str(app))
    command("xcrun", "stapler", "validate", str(app))
    command("spctl", "--assess", "--type", "execute", "--verbose=2", str(app))
    app_details = command("codesign", "-dv", "--verbose=4", str(app)).stderr
    team = re.search(r"(?m)^TeamIdentifier=([A-Z0-9]{10})$", app_details)
    require(team and "Authority=Developer ID Application:" in app_details,
            "outer Developer ID signature required")
    runtime = app / RUNTIME
    require(runtime.is_file() and not runtime.is_symlink(), "fixed bundled runtime missing or linked")
    provenance = json.loads((app / PROVENANCE).read_text())
    require(provenance["version"] == "2.1.6" and provenance["signed"] is True,
            "expected signed Syncthing 2.1.6 provenance")
    # The pre-signing archive pin is checked by the source's stage operation;
    # post-signing bytes necessarily differ and must match its sealed receipt.
    pins = {"arm64": "f86bfa0bdbb5b1b13229a7e5643e898e3c4e017a4fff6dd525994ebaf28ff6e2",
            "amd64": "d1464be24a7157440dfb52e2d84939f83caf89c129761945abcdd4956e09ffb1"}
    require(provenance["archive_sha256"] == pins.get(provenance["architecture"]),
            "upstream archive digest is not pinned")
    require(sha(runtime) == provenance["signed_binary_sha256"], "signed runtime digest mismatch")
    command("codesign", "--verify", "--strict", str(runtime))
    details = command("codesign", "-dv", "--verbose=4", str(runtime)).stderr
    for expected in ("Identifier=" + plist["CFBundleIdentifier"] + ".syncthing\n",
                     "TeamIdentifier=" + team[1] + "\n", "Authority=Developer ID Application:"):
        require(expected in details, "nested signature identity mismatch")
    require("runtime" in details and re.search(r"(?m)^Timestamp=.+$", details),
            "nested hardened runtime and secure timestamp required")
    # Explicit read-only command; no serve/config generation/UI launch.
    version = command(str(runtime), "--version", timeout=15).stdout.strip()
    require(re.match(r"^syncthing v2\.1\.6(?:\s|$)", version), "bundled runtime version mismatch")
    return {"source": source, "verify_only": True, "publication": "unreachable",
            "notarization": notarization, "team": team[1], "nested_signature": details,
            "runtime_version": version, "runtime_sha256": sha(runtime),
            "provenance": provenance,
            "artifacts": {p.name: {"sha256": sha(p), "size": p.stat().st_size}
                          for p in output.iterdir() if p.suffix in (".zip", ".dmg")}}


def execute(env, api=api_get):
    source = admission(env, api)
    require(sys.platform == "darwin", "native macOS runner required")
    # A full 90-minute job must finish before the existing 02:20 night guard.
    # Native verification never competes with the 03:05 beta or its catch-up lane.
    now = datetime.now(ZoneInfo("Asia/Jerusalem")).time()
    require(not time(0, 50) <= now < time(9), "night Mac beta has priority; verify deferred")
    root = Path(env["GITHUB_WORKSPACE"]).resolve()
    checkout = root / "verify-product"
    output = Path(env["RUNNER_TEMP"]) / "native-verify"
    require(not checkout.exists() and not output.exists(), "ambiguous existing verify output")
    require(command("git", "rev-parse", "HEAD", cwd=root).stdout.strip() == env["GITHUB_SHA"],
            "trusted workflow checkout mismatch")
    command("git", "init", "-q", str(checkout))
    command("git", "-C", str(checkout), "remote", "add", "origin",
            "https://git.oklabs.uk/" + REPO + ".git")
    command("git", "-C", str(checkout), "fetch", "--no-tags", "--depth", "1",
            "origin", source, timeout=120)
    command("git", "-C", str(checkout), "checkout", "--detach", "FETCH_HEAD")
    require(command("git", "rev-parse", "HEAD", cwd=checkout).stdout.strip() == source,
            "product checkout mismatch")
    # Fail closed even if publication credentials were accidentally added later.
    forbidden = re.compile(r"R2_|SPARKLE_ED_PRIVATE_KEY|GITHUB_TOKEN|FORGEJO_TOKEN")
    build_env = {key: value for key, value in env.items() if not forbidden.search(key)}
    build_env["OKILUM_BUILD_VERSION"] = str(5000 + int(env["GITHUB_RUN_NUMBER"]))
    # Packaging's existing script signs/notarizes/staples locally and emits files.
    # There is deliberately no publication module, API client, or feed-writing step.
    subprocess.run(["bash", "scripts/build-macos-ci.sh", str(output)],
                   cwd=checkout, env=build_env, check=True, timeout=80 * 60)
    receipt = verify_artifact(output, source)
    (output / "native-verify-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))


if __name__ == "__main__":
    execute(dict(os.environ))
