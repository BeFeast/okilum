#!/usr/bin/env python3
"""Ordered CT119 Inbox deployment. Run on CT119, never against the real vault."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess
import tarfile
import time
import urllib.request
import uuid


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
    def __init__(self, compose_dir, state, origin, timeout=120):
        self.compose_dir = compose_dir.resolve()
        self.state = state.resolve()
        self.origin = origin
        self.timeout = timeout
        self.active = self.state / "active-image.json"
        self.nginx = self.compose_dir / "nginx.conf"

    def run(self, args):
        # Compose config and daemon errors may contain environment values: retain no output.
        result = subprocess.run(args, cwd=self.compose_dir, capture_output=True,
                                text=True, timeout=300)
        if result.returncode:
            raise RuntimeError("Docker operation failed (output suppressed to protect configuration)")
        return result.stdout.strip()

    def compose(self, *args):
        command = ["docker", "compose", "--project-directory", str(self.compose_dir),
                   "-f", str(self.compose_dir / "compose.yml")]
        override = self.compose_dir / "compose.override.yml"
        if override.exists():
            command += ["-f", str(override)]
        if self.active.exists():
            command += ["-f", str(self.active)]
        return self.run(command + list(args))

    def images(self):
        images = {}
        for service in ("inbox", "ingress"):
            container = self.compose("ps", "-q", service)
            if not container or "\n" in container:
                raise RuntimeError("Expected one running Inbox and ingress container")
            images[service] = self.run(["docker", "inspect", "--format", "{{.Image}}", container])
        return images

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

    def snapshot(self, directory, images):
        self.compose("exec", "-T", "inbox", "/usr/local/bin/inbox-backup")
        self.compose("cp", "inbox:/backups/inbox-latest.db", str(directory / "inbox.db"))
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
            "env_file": str(self.compose_dir / ".env"),
            "credentials": "External references remain in the existing Compose configuration"}))

    def deploy(self, image=None, nginx=None):
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
            self.ordered_start()
        except BaseException:
            record("rolling_back")
            try:
                self.nginx.write_bytes((directory / "nginx.conf").read_bytes())
                self.select_images(old)
                self.ordered_start()
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
    state = args.state_dir.resolve()
    source = args.compose_dir.resolve().parent.parent
    if state.is_relative_to(source):
        parser.error("state directory must be outside the source tree")
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    if state.stat().st_mode & 0o077:
        parser.error("state directory must be private (mode 0700)")
    with (state / "deploy.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        deployment = Deployment(args.compose_dir, state, args.origin)
        print(json.dumps(deployment.deploy(args.image, args.nginx_config)))


if __name__ == "__main__":
    main()
