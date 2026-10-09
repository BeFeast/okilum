#!/usr/bin/env python3
"""Ordered CT119 Inbox deployment. Run on CT119, never against the real vault."""
import argparse
from contextlib import closing
import fcntl
import json
import os
import signal
import sqlite3
import tempfile
from pathlib import Path
import subprocess
import tarfile
import time
import urllib.request
import uuid
from invocation import Invocation


class OutageTimeout(Exception):
    """Whole-start deadline; deliberately not a retryable HTTP/OSError."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def public_ready(origin, opener=None):
    """No owner login: discard the anonymous flow cookie and challenge body."""
    opener = opener or urllib.request.build_opener(NoRedirect)
    for path, data in [("/", None), ("/api/v1/auth/login/start", b"{}")]:
        request = urllib.request.Request(origin + path, data=data, headers={
            "Origin": origin, "Content-Type": "application/json"})
        with opener.open(request, timeout=10) as response:
            if response.status != 200:
                raise RuntimeError("Public readiness did not return HTTP 200")
            if data is not None:
                options = json.loads(response.read(1024 * 1024))
                key = options.get("publicKey", {}) if isinstance(options, dict) else {}
                if not isinstance(key, dict) or not key.get("challenge"):
                    raise RuntimeError("Public login response has no WebAuthn challenge")


class Deployment:
    def __init__(self, compose_dir, state, origin, timeout=120, outage_timeout=90, invocation=None):
        self.compose_dir = compose_dir.resolve()
        self.state = state.resolve()
        self.origin = origin
        if invocation is None:
            raise ValueError("Existing activate and rollback script invocation is required")
        self.invocation = invocation
        self.working_dir = invocation.cwd
        self.timeout = timeout
        self.outage_timeout = outage_timeout
        self.active = self.state / "active-image.json"
        self.nginx = self.compose_dir / "nginx.conf"

    def run(self, args):
        # Compose config and daemon errors may contain environment values: retain no output.
        result = subprocess.run(args, cwd=self.working_dir, capture_output=True,
                                text=True, timeout=300)
        if result.returncode:
            raise RuntimeError("Docker operation failed (output suppressed to protect configuration)")
        return result.stdout.strip()

    def compose_command(self, *args):
        return self.invocation.command(self.active, *args)

    def compose(self, *args):
        return self.run(self.compose_command(*args))

    def copy_backup(self, destination):
        # The caller writes the private copy: sudo docker cp would create a
        # root-owned 0600 file that an unprivileged operator cannot restore-check.
        with destination.open("xb") as output:
            result = subprocess.run(self.compose_command("exec", "-T", "inbox",
                "cat", "/backups/inbox-latest.db"), stdout=output,
                stderr=subprocess.DEVNULL, cwd=self.working_dir, timeout=300)
        if result.returncode:
            raise RuntimeError("Backup export failed")

    def images(self):
        images = {}
        for service in ("inbox", "ingress"):
            container = self.compose("ps", "-q", service)
            if not container or "\n" in container:
                raise RuntimeError("Expected one running Inbox and ingress container")
            images[service] = self.run(["docker", "inspect", "--format", "{{.Image}}", container])
        return images

    @staticmethod
    def validate_runtime(service, desired, image, actual):
        # Values stay in memory; mismatch errors must not expose credentials.
        for field, key in (("command", "Cmd"), ("entrypoint", "Entrypoint")):
            expected = desired.get(field)
            if expected is None:
                expected = image.get(key)
            if expected != actual.get(key):
                raise RuntimeError(f"{service}: resolved {field} differs from running container; stop")
        actual_env = dict(item.split("=", 1) for item in actual.get("Env", []) if "=" in item)
        expected_env = dict(item.split("=", 1) for item in image.get("Env", []) if "=" in item)
        for key, value in desired.get("environment", {}).items():
            if value is None:
                expected_env.pop(key, None)
            else:
                expected_env[key] = str(value)
        if expected_env != actual_env:
            raise RuntimeError(f"{service}: resolved environment differs from running container; stop")

    def validate_topology(self, service, resolved, actual):
        desired = resolved["services"][service]
        expected_mounts = {}
        for mount in desired.get("volumes", []):
            kind, source = mount["type"], mount.get("source")
            if kind == "bind":
                source = str(Path(source).resolve())
            elif kind == "volume" and source:
                source = resolved["volumes"][source]["name"]
            else:
                raise RuntimeError("Unsupported mount identity; stop")
            expected_mounts[mount["target"]] = (kind, source, not mount.get("read_only", False))
        current_mounts = {}
        for mount in actual.get("Mounts", []):
            if mount["Type"] == "tmpfs":
                continue
            source = str(Path(mount["Source"]).resolve()) if mount["Type"] == "bind" else mount.get("Name")
            current_mounts[mount["Destination"]] = (mount["Type"], source, mount["RW"])
        if expected_mounts != current_mounts:
            raise RuntimeError(f"{service}: resolved mounts differ from running container; stop")
        host = actual["HostConfig"]
        if bool(desired.get("read_only", False)) != host.get("ReadonlyRootfs", False):
            raise RuntimeError(f"{service}: root filesystem mode differs; stop")
        expected_tmpfs = dict(item.split(':', 1) if ':' in item else (item, '')
                              for item in desired.get('tmpfs', []))
        if expected_tmpfs != (host.get('Tmpfs') or {}):
            raise RuntimeError(f"{service}: tmpfs configuration differs; stop")
        expected_ports = sorted((str(p["target"])+"/"+p.get("protocol", "tcp"),
            p.get("host_ip") or "0.0.0.0", str(p.get("published", ""))) for p in desired.get("ports", []))
        current_ports = sorted((port, binding.get("HostIp") or "0.0.0.0", binding["HostPort"])
            for port, bindings in (host.get("PortBindings") or {}).items() for binding in (bindings or []))
        if expected_ports != current_ports:
            raise RuntimeError(f"{service}: resolved port bindings differ; stop")
        mode = desired.get("network_mode")
        if mode and mode.startswith("service:"):
            owner = self.compose("ps", "-q", mode.split(":", 1)[1])
            if host.get("NetworkMode") != "container:"+owner:
                raise RuntimeError(f"{service}: shared network namespace differs; stop")
        elif mode:
            if host.get("NetworkMode") != mode:
                raise RuntimeError(f"{service}: network mode differs; stop")
        else:
            networks = {resolved["networks"][name]["name"] for name in desired.get("networks", {})}
            if networks != set(actual.get("NetworkSettings", {}).get("Networks", {})):
                raise RuntimeError(f"{service}: resolved networks differ; stop")

    def preflight(self):
        for env_file in self.invocation.env_files:
            if not env_file.is_file() or env_file.stat().st_mode & 0o077:
                raise RuntimeError("Script env file must exist and be private")
            if env_file.is_relative_to(self.compose_dir.parent.parent):
                raise RuntimeError("Keep runtime env files outside the archived source tree")
        resolved = json.loads(self.compose("config", "--format", "json"))
        for service in ("inbox", "ingress"):
            container = self.compose("ps", "-q", service)
            actual = json.loads(self.run(["docker", "inspect", container]))[0]
            image = json.loads(self.run(["docker", "image", "inspect", actual["Image"]]))[0]
            self.invocation.verify_labels(actual["Config"].get("Labels", {}), self.active, resolved["name"])
            self.validate_runtime(service, resolved["services"][service], image["Config"], actual["Config"])
            self.validate_topology(service, resolved, actual)
        public_ready(self.origin)

    def select_images(self, images):
        data = {"services": {name: {"image": image} for name, image in images.items()}}
        data["services"]["init"] = {"image": images["inbox"]}
        temporary = self.active.with_suffix(".tmp")
        temporary.write_text(json.dumps(data))
        temporary.replace(self.active)

    def ordered_start(self):
        # A stopped ingress still refers to the old namespace. Remove that stateless
        # container before recreating its namespace owner; never remove volumes.
        self.compose("rm", "--stop", "--force", "ingress")
        self.compose("up", "-d", "--no-deps", "--force-recreate", "--pull", "never", "inbox")
        deadline = time.monotonic() + self.timeout
        while True:
            container = self.compose("ps", "-q", "inbox")
            if container:
                health = self.run(["docker", "inspect", "--format",
                                   "{{if .State.Health}}{{.State.Health.Status}}{{end}}", container])
                if health == "healthy":
                    break
            if time.monotonic() >= deadline:
                raise RuntimeError("Inbox health deadline expired")
            time.sleep(1)
        self.compose("up", "-d", "--no-deps", "--force-recreate", "--pull", "never", "ingress")
        deadline = time.monotonic() + self.timeout
        while True:
            try:
                public_ready(self.origin)
                return
            except (OSError, ValueError, RuntimeError):
                if time.monotonic() >= deadline:
                    raise RuntimeError("Public HTTPS/WebAuthn readiness deadline expired") from None
                time.sleep(2)

    def bounded_start(self):
        # One deadline covers Docker, health and public checks, not each phase.
        def expired(signum, frame):
            raise OutageTimeout("Outage budget expired; rollback required")
        previous = signal.signal(signal.SIGALRM, expired)
        signal.setitimer(signal.ITIMER_REAL, self.outage_timeout)
        try:
            self.ordered_start()
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)

    @staticmethod
    def verify_restore(snapshot):
        # Restore only to a disposable DB, never over live operational state.
        with tempfile.TemporaryDirectory(prefix="restore-check-", dir=snapshot.parent) as temp:
            with closing(sqlite3.connect(snapshot.resolve().as_uri() + "?mode=ro", uri=True)) as saved:
                with closing(sqlite3.connect(str(Path(temp) / "restored.db"))) as restored:
                    saved.backup(restored)
                    if restored.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                        raise RuntimeError("Restored backup integrity check failed")
                    for table in ("captures", "capture_operations", "auth_owner",
                                  "discussion_turns", "publications"):
                        before = saved.execute(f'SELECT count(*) FROM {table}').fetchone()
                        after = restored.execute(f'SELECT count(*) FROM {table}').fetchone()
                        if before != after:
                            raise RuntimeError("Restored backup count mismatch")

    def snapshot(self, directory, images):
        self.compose("exec", "-T", "inbox", "/usr/local/bin/inbox-backup")
        self.copy_backup(directory / "inbox.db")
        self.verify_restore(directory / "inbox.db")
        self.run(["docker", "image", "save", "--output", str(directory / "images.tar"),
                  *sorted(set(images.values()))])
        # Back up source, including runtime Compose overrides, without following
        # symlinks or collecting credentials, build caches, or the live vault/DB.
        source = self.compose_dir.parent.parent
        def include(info):
            if any(p in {".git", "target", "node_modules", "__pycache__", ".env", "secrets",
                             "data", "backups", "fixture-vault"}
                   for p in Path(info.name).parts):
                return None
            return info
        with tarfile.open(directory / "source.tar.gz", "w:gz", dereference=False) as archive:
            archive.add(source, arcname="source", filter=include)
        (directory / "nginx.conf").write_bytes(self.nginx.read_bytes())
        (directory / "images.json").write_text(json.dumps(images))
        (directory / "configuration-references.json").write_text(json.dumps({
            "compose_directory": str(self.compose_dir),
            "env_files": [str(p) for p in self.invocation.env_files],
            "compose_argv": list(self.invocation.argv),
            "working_dir": str(self.working_dir),
            "invocation_scripts": [str(p) for p in self.invocation.scripts],
            "credentials": "External references remain in the existing Compose configuration"}))

    def deploy(self, image=None, nginx=None):
        self.preflight()
        old = self.images()
        desired = dict(old)
        if image:
            desired["inbox"] = self.run(["docker", "image", "inspect", "--format", "{{.Id}}", image])
        config = nginx.read_bytes() if nginx else None
        directory = self.state / ("rollback-" + uuid.uuid4().hex)
        directory.mkdir(mode=0o700)
        # No service or active configuration mutation before all backups succeed.
        self.snapshot(directory, old)
        receipt = {"backup": str(directory), "status": "prepared", "origin": self.origin}
        def record(status):
            receipt["status"] = status
            (directory / "receipt.json").write_text(json.dumps(receipt, indent=2))
        record("prepared")
        try:
            self.select_images(desired)
            if config is not None:
                # Keep the mounted inode until ingress is recreated.
                self.nginx.write_bytes(config)
            self.bounded_start()
        except BaseException:
            record("rolling_back")
            try:
                self.nginx.write_bytes((directory / "nginx.conf").read_bytes())
                self.select_images(old)
                self.bounded_start()
                record("rolled_back_public_ready")
            except BaseException:
                record("rollback_failed")
                raise RuntimeError(f"Rollback failed; preserve backup and inspect {directory}") from None
            raise RuntimeError(f"Deployment failed; previous services publicly ready; backup {directory}") from None
        record("deployed_public_ready")
        return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compose-dir", type=Path, default=Path("/opt/tessera-inbox/source/inbox/deploy"))
    parser.add_argument("--state-dir", type=Path, default=Path("/opt/tessera-inbox/deployment-state"))
    parser.add_argument("--origin", default="https://inbox-qa.oklabs.uk")
    parser.add_argument("--activate-script", type=Path)
    parser.add_argument("--rollback-script", type=Path)
    parser.add_argument("--image", help="Already-loaded Inbox image tag or digest; never pulled by this tool")
    parser.add_argument("--nginx-config", type=Path)
    parser.add_argument("--check", action="store_true", help="Public readiness only, without Docker or restarts")
    args = parser.parse_args()
    from urllib.parse import urlsplit
    url = urlsplit(args.origin)
    if url.scheme != "https" or not url.netloc or url.path or url.query or url.fragment or url.username:
        parser.error("origin must be a plain HTTPS origin")
    os.umask(0o077)
    if args.check:
        public_ready(args.origin)
        print("Public HTTPS and WebAuthn challenge ready")
        return
    if not args.activate_script or not args.rollback_script:
        parser.error("--activate-script and --rollback-script are required for deployment")
    invocation = Invocation.from_scripts(args.activate_script, args.rollback_script)
    if args.compose_dir.resolve() != invocation.files[0].parent:
        parser.error("compose directory differs from script source files")
    state = args.state_dir.resolve()
    source = args.compose_dir.resolve().parent.parent
    if state.is_relative_to(source):
        parser.error("state directory must be outside the source tree")
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    if state.stat().st_mode & 0o077:
        parser.error("state directory must be private (mode 0700)")
    with (state / "deploy.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        deployment = Deployment(args.compose_dir, state, args.origin, invocation=invocation)
        print(json.dumps(deployment.deploy(args.image, args.nginx_config)))


if __name__ == "__main__":
    main()
