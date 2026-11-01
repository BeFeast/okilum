#!/usr/bin/env python3
"""Pinned offline-verifiable macOS Syncthing payload; never starts the daemon."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent
VERSION = "2.1.6"
FINGERPRINT = "FBA2E162F2F44657B38F0309E5665F9BD5970C47"
LEGACY = "D26E6ED000654A3E"
BASE = "https://github.com/syncthing/syncthing/releases/download/v" + VERSION
PINS = {
    "arm64": "f86bfa0bdbb5b1b13229a7e5643e898e3c4e017a4fff6dd525994ebaf28ff6e2",
    "amd64": "d1464be24a7157440dfb52e2d84939f83caf89c129761945abcdd4956e09ffb1",
}
CPU = {"arm64": 0x100000c, "amd64": 0x1000007}
RUNTIME = Path("Contents/MacOS/syncthing")
PROVENANCE = Path("Contents/Resources/Syncthing/provenance.json")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def signature_status(status):
    lines = [line.split() for line in status.splitlines() if line.startswith("[GNUPG:] ")]
    require(not any(p[1] in {"BADSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG"} for p in lines),
            "upstream checksum signature rejected")
    require(any(p[1] == "VALIDSIG" and p[2] == FINGERPRINT for p in lines),
            "pinned upstream checksum signature missing")
    unknown = [p[2] for p in lines if p[1] == "NO_PUBKEY"]
    require(all(key == LEGACY for key in unknown), "unexpected missing signing key")
    return {"validsig": FINGERPRINT, "expected_legacy_no_pubkey": unknown}


def verify_upstream():
    # A disposable keyring; no user's trust database, keyserver, or network GPG.
    with tempfile.TemporaryDirectory() as home:
        listing = run("gpg", "--batch", "--homedir", home, "--with-colons",
                      "--show-keys", str(ROOT / "release-key.asc")).stdout
        records = listing.splitlines()
        fingerprints = [records[i + 1].split(":")[9] for i, line in enumerate(records)
                        if line.startswith("pub:")]
        require(fingerprints == [FINGERPRINT], "release-key fingerprint changed")
        run("gpg", "--batch", "--homedir", home, "--import", str(ROOT / "release-key.asc"))
        checked = subprocess.run(["gpg", "--batch", "--homedir", home, "--no-auto-key-retrieve",
                                  "--status-fd", "1", "--verify",
                                  str(ROOT / "sha256sum-v2.1.6.txt.asc")],
                                 capture_output=True, text=True, errors="replace", timeout=30)
        proof = signature_status(checked.stdout)
    text = (ROOT / "sha256sum-v2.1.6.txt.asc").read_text()
    for arch, pin in PINS.items():
        require(re.search(r"(?m)^" + pin + r"  syncthing-macos-" + arch
                          + r"-v2\.1\.6\.zip$", text), "archive pin differs from signed list")
    return proof


def archive_payload(archive, arch):
    data = Path(archive).read_bytes()
    require(digest(data) == PINS[arch], "Syncthing archive SHA256 mismatch")
    prefix = "syncthing-macos-" + arch + "-v" + VERSION + "/"
    with zipfile.ZipFile(archive) as zipped:
        names = zipped.namelist()
        require(len(names) == len(set(names)), "duplicate archive entry")
        result = {}
        for name in ("syncthing", "LICENSE.txt", "AUTHORS.txt", "README.txt"):
            entry = zipped.getinfo(prefix + name)
            require((entry.external_attr >> 16) & 0o170000 != 0o120000,
                    "linked archive payload")
            require(entry.file_size < 100_000_000, "oversized payload")
            result[name] = zipped.read(entry)
    binary = result["syncthing"]
    require(len(binary) > 32 and binary[:4] == b"\xcf\xfa\xed\xfe"
            and struct.unpack_from("<I", binary, 4)[0] == CPU[arch],
            "Syncthing payload is not the expected thin Mach-O architecture")
    return result


def stage(app, arch, archive=None):
    proof = verify_upstream()
    name = "syncthing-macos-" + arch + "-v" + VERSION + ".zip"
    with tempfile.TemporaryDirectory() as tmp:
        if archive is None:
            archive = Path(tmp) / name
            with urllib.request.urlopen(BASE + "/" + name, timeout=60) as response:
                archive.write_bytes(response.read(100_000_001))
        payload = archive_payload(archive, arch)
    destination = app / RUNTIME
    require(not destination.exists() and not destination.is_symlink(), "runtime already staged")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(payload["syncthing"])
    destination.chmod(0o755)
    resources = app / PROVENANCE.parent
    resources.mkdir(parents=True, exist_ok=False)
    for name in ("LICENSE.txt", "AUTHORS.txt", "README.txt"):
        (resources / name).write_bytes(payload[name])
    for name in ("release-key.asc", "sha256sum-v2.1.6.txt.asc"):
        (resources / name).write_bytes((ROOT / name).read_bytes())
    provenance = dict(schema=1, version=VERSION, architecture=arch,
                      archive_url=BASE + "/" + "syncthing-macos-" + arch + "-v" + VERSION + ".zip",
                      archive_sha256=PINS[arch], upstream_binary_sha256=digest(payload["syncthing"]),
                      checksum_signature=proof, source_url="https://github.com/syncthing/syncthing/tree/v2.1.6",
                      source_archive_url=BASE + "/syncthing-source-v2.1.6.tar.gz",
                      license="MPL-2.0", signed=False)
    (app / PROVENANCE).write_text(json.dumps(provenance, indent=2) + "\n")


def sign(app, identity, identifier, team=None):
    require(re.fullmatch(r"[A-Za-z0-9_.-]+", identifier), "invalid bundle identifier")
    path = app / RUNTIME
    provenance = json.loads((app / PROVENANCE).read_text())
    require(digest(path.read_bytes()) == provenance["upstream_binary_sha256"],
            "staged runtime changed before signing")
    args = ["codesign", "--force", "--options", "runtime", "--identifier", identifier + ".syncthing"]
    if identity != "-":
        require(team and re.fullmatch(r"[A-Z0-9]{10}", team), "release Team ID required")
        args.append("--timestamp")
    if keychain := os.environ.get("OKILUM_SIGNING_KEYCHAIN"):
        args += ["--keychain", keychain]
    run(*args, "--sign", identity, str(path))
    run("codesign", "--verify", "--strict", str(path))
    details = run("codesign", "-dv", "--verbose=4", str(path)).stderr
    require("Identifier=" + identifier + ".syncthing\n" in details, "nested identifier mismatch")
    if identity != "-":
        require("TeamIdentifier=" + team + "\n" in details, "nested Team mismatch")
        require("Authority=Developer ID Application:" in details, "Developer ID required")
    require("runtime" in details, "nested hardened runtime missing")
    provenance.update(signed=identity != "-", signed_binary_sha256=digest(path.read_bytes()),
                      code_identifier=identifier + ".syncthing", team=team,
                      codesign_details=details)
    (app / PROVENANCE).write_text(json.dumps(provenance, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("stage", "sign", "verify-upstream"))
    parser.add_argument("--app", type=Path)
    parser.add_argument("--arch", choices=PINS)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--identity")
    parser.add_argument("--identifier")
    parser.add_argument("--team")
    args = parser.parse_args()
    if args.mode == "verify-upstream":
        print(json.dumps(verify_upstream()))
    else:
        require(args.app is not None, "app required")
        if args.mode == "stage":
            require(args.arch in PINS, "architecture required")
            stage(args.app, args.arch, args.archive)
        else:
            sign(args.app, args.identity, args.identifier, args.team)


if __name__ == "__main__":
    main()
