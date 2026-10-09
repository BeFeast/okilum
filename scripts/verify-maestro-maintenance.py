#!/usr/bin/env python3
"""Verify local cored executables against an isolated brain and provider fixture.

The default mode checks NEW_LINKS_ENABLED=false. The optional --guarded mode
checks NEW_DECISIONS_ENABLED=false with the same journal schema, actual Go wire
fixtures and a pre-control downgrade-refusal binary. No installed service,
personal brain, or external provider is contacted.
"""
import argparse
import contextlib
import datetime
import hashlib
import http.server
import json
import pathlib
import selectors
import socket
import subprocess
import tempfile
import threading
import time
import uuid
import urllib.parse


def uid():
    return str(uuid.uuid4())


def digest(path):
    with pathlib.Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Fixture(http.server.BaseHTTPRequestHandler):
    methods = []
    generation = 1

    def log_message(self, *_):
        pass

    def parse_request(self):
        parsed = super().parse_request()
        if parsed:
            self.methods.append((self.command, self.path))
        return parsed

    def do_GET(self):
        assert self.path == "/api/v1/fleet"
        now = datetime.datetime.now(datetime.timezone.utc).isoformat()
        body = json.dumps({
            "refreshed_at": now,
            "projects": [{
                "project_id": "01000000-0000-4000-8000-000000000152",
                "name": "fixture", "repo": "fixture/maintenance", "paused": True,
                "freshness": {"snapshot_age_seconds": 0, "stale_after_seconds": 900},
            }],
            "workers": [{
                "project_name": "fixture", "project_repo": "fixture/maintenance",
                "slot": "fixture-1", "worker_generation": self.generation,
                "started_at": "2026-09-01T00:00:00Z", "issue_number": 42,
                "issue_title": "Maintenance fixture",
                "issue_url": "https://example.test/fixture/maintenance/issues/42",
                "status": "running", "live": True,
            }],
            "approvals": [],
        }).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.send_error(405)


def positive_control(server, fixture):
    """A forbidden verb must appear in the same trace used by absence assertions."""
    with socket.create_connection(("127.0.0.1", server.server_port), timeout=3) as connection:
        connection.sendall(b"PATCH /api/v1/fleet HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        while connection.recv(4096):
            pass
    assert fixture.methods == [("PATCH", "/api/v1/fleet")], fixture.methods
    fixture.methods.clear()


class Backend:
    def __init__(self, binary, root, brain_id):
        self.stderr = (root / ("backend-" + uid() + ".log")).open("wb")
        self.child = subprocess.Popen([
            str(binary), "brain", "--brain-id", brain_id,
            "--listen", "127.0.0.1:0", "--records-dir", "records",
            "--managed-brain", "--vault", str(root / "brain"),
            "--operational-dir", str(root / "runtime"),
        ], stdout=subprocess.PIPE, stderr=self.stderr)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(self.child.stdout, selectors.EVENT_READ)
                assert selector.select(15), "backend did not become ready"
            ready = json.loads(self.child.stdout.readline())
            assert ready["ready"] is True
            host, port = ready["listen"].rsplit(":", 1)
            self.address = (host, int(port))
            self.workspace = self.call("capabilities")["workspace"]
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(5)
        self.child.stdout.close()
        self.stderr.close()

    def request(self, op, **fields):
        payload = {"schema": "ai-brain/v1", "id": uid(), "op": op, **fields}
        if hasattr(self, "workspace"):
            payload["expected_workspace"] = self.workspace
        with socket.create_connection(self.address, timeout=7) as connection:
            connection.sendall(json.dumps(payload).encode() + b"\n")
            with connection.makefile("rb") as stream:
                return json.loads(stream.readline())

    def call(self, op, **fields):
        reply = self.request(op, **fields)
        assert reply["ok"], reply
        return reply["data"]

    def create_goal(self):
        goal = uid()
        self.call("create_goal", goal={
            "id": goal, "title": "Isolated maintenance fixture", "status": "active",
            "criteria": [{"id": "C1", "description": "Evidence still needs verification", "requires_human": False}],
            "stage_ids": [], "task_ref": None,
        }, body="# Maintenance fixture\n")
        return goal


def verify(enabled, maintenance):
    Fixture.methods = []
    Fixture.generation = 1
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        positive_control(server, Fixture)
        with tempfile.TemporaryDirectory(prefix="okilum-maestro-maintenance-") as temporary:
            root = pathlib.Path(temporary)
            (root / "brain/records").mkdir(parents=True)
            (root / "runtime").mkdir()
            brain_id = uid()
            config = {
                "actor": "fixture operator", "chat": None, "todoist": None, "t3": None,
                "maestro": {"base_url": f"http://127.0.0.1:{server.server_port}", "instance_id": uid(), "token_env": None, "ui_origin": None},
            }
            with contextlib.closing(Backend(enabled, root, brain_id)) as backend:
                goal = backend.create_goal()
                backend.call("connectors_save", config=config)
                assert backend.call("capabilities")["maestro_link"] is True
                choice = backend.call("maestro_discover")["projects"][0]
                request = {
                    "operation_id": uid(), "goal_id": goal,
                    "selection_guard": choice["issues"][0]["selection_guard"],
                    "project_id": choice["project_id"], "project_name": choice["name"],
                    "repo": choice["repo"], "issue_number": 42,
                }
                linked = backend.call("maestro_link", **request)
                before = backend.call("maestro_get", goal_id=goal)
                goal_path = root / "brain" / backend.call("snapshot", goal_id=goal)["source_paths"]["goal"]
                goal_sha = digest(goal_path)
            with contextlib.closing(Backend(maintenance, root, brain_id)) as backend:
                assert backend.call("capabilities")["maestro_link"] is False
                assert backend.call("capabilities")["maestro_observation"] is True
                assert backend.call("connectors_get")["config"] == config
                current = backend.call("maestro_get", goal_id=goal)
                assert current["link"]["id"] == linked["link_id"]
                assert current["source_paths"] == before["source_paths"]
                assert backend.call("maestro_link", **request) == linked
                another = backend.create_goal()
                refused = backend.request("maestro_link", **{**request, "goal_id": another, "operation_id": uid()})
                assert refused["ok"] is False and "disabled" in refused["error"]["message"], refused
                Fixture.generation = 2
                deadline = time.monotonic() + 25
                while True:
                    current = backend.call("maestro_get", goal_id=goal)
                    if current["link"]["latest"]["issue"]["attempts"][0]["generation"] == 2:
                        break
                    assert time.monotonic() < deadline, "maintenance observer did not retain new generation"
                    time.sleep(0.2)
                assert len(current["link"]["observation_ids"]) == 2
                assert digest(goal_path) == goal_sha
                assert backend.call("snapshot", goal_id=goal)["stages"] == []
                server.shutdown()
                server.server_close()
                thread.join(3)
                # A genuine offline replay must not depend on a fresh GET succeeding.
                assert backend.call("maestro_link", **request) == linked
                unlink_request = {"operation_id": uid(), "goal_id": goal, "expected_link_id": linked["link_id"]}
                unlinked = backend.call("maestro_unlink", **unlink_request)
                assert backend.call("maestro_unlink", **unlink_request) == unlinked
            with contextlib.closing(Backend(maintenance, root, brain_id)) as backend:
                current = backend.call("maestro_get", goal_id=goal)
                assert current["link"] is None
                assert len(current["history"]) == 1 and not current["history"][0]["active"]
                assert len(current["source_paths"]["observations"]) == 2
                assert backend.call("maestro_unlink", **unlink_request) == unlinked
                assert digest(goal_path) == goal_sha
            assert Fixture.methods and all(method == "GET" and path == "/api/v1/fleet" for method, path in Fixture.methods)
            return {
                "schema": "okilum-maestro-maintenance-verification/v1", "passed": True,
                "enabled_sha256": digest(enabled), "maintenance_sha256": digest(maintenance),
                "fixture_requests": len(Fixture.methods), "remote_mutations": 0,
                "verified": ["non_get_detection_positive_control", "saved_settings", "restart_link_identity", "source_paths", "offline_link_replay", "new_link_refused", "new_generation_observed", "goal_unchanged", "no_stage", "unlink_replay_after_restart"],
            }
    finally:
        server.shutdown()
        server.server_close()
        thread.join(3)


class GuardedFixture(Fixture):
    """Replay actual Go handler wire values, with a deliberate lost POST reply."""
    pending = None
    decided = None
    reveal_receipt = False
    posts = []

    def do_GET(self):
        assert self.path == "/api/v1/fleet"
        raw = json.loads(json.dumps(self.decided if self.reveal_receipt else self.pending))
        # Only observation freshness advances. Exact guarded review and receipt
        # bytes retain their original Go-generated identity/mint/revision.
        raw["refreshed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        for project in raw["projects"]:
            project["freshness"]["snapshot_age_seconds"] = 0
        body = json.dumps(raw).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        type(self).posts.append({"path": self.path, "body": body})
        self.close_connection = True
        self.connection.shutdown(socket.SHUT_RDWR)
        self.connection.close()


def guarded_capabilities(capabilities, approval_send):
    """Check actual RPC values only after the managed Maestro config is saved."""
    expected = {"maestro_observation": True, "maestro_link": True,
                "maestro_control": True, "maestro_approval_send": approval_send}
    observed = {name: capabilities.get(name) for name in expected}
    for name, value in expected.items():
        assert observed[name] is value, (
            f"configured Maestro capability {name}: expected {value!r}, got {observed[name]!r}")
    return observed


def capability_negative_control(capabilities):
    """The same assertion must reject the false-control fixture used by #237."""
    mismatched = {**capabilities, "maestro_control": False}
    expected_error = "configured Maestro capability maestro_control: expected True, got False"
    try:
        guarded_capabilities(mismatched, approval_send=True)
    except AssertionError as error:
        assert str(error) == expected_error, error
        return {"changed_capability": "maestro_control", "injected_value": False,
                "rejected": True, "error": str(error)}
    raise AssertionError("incorrect configured Maestro capability fixture was accepted")


def verify_guarded(enabled, maintenance, predecessor, wire_fixture, root):
    """Use the existing backend/fixture seam for real maintenance compatibility."""
    assert predecessor, "guarded compatibility requires a pre-control binary"
    root.mkdir(parents=True, exist_ok=True)
    assert not (root / "brain").exists() and not (root / "runtime").exists(), "fixture already enrolled"
    (root / "brain/records").mkdir(parents=True)
    (root / "runtime").mkdir()
    binary_hashes = {"enabled": digest(enabled), "maintenance": digest(maintenance), "predecessor": digest(predecessor)}
    provenance = json.loads((wire_fixture / "provenance.json").read_text())
    for name, expected_hash in provenance["files"].items():
        assert digest(wire_fixture / name) == expected_hash, f"Go fixture changed: {name}"
    GuardedFixture.pending = json.loads((wire_fixture / "fleet-pending.json").read_text())
    GuardedFixture.decided = json.loads((wire_fixture / "fleet-receipt-readonly.json").read_text())
    GuardedFixture.methods, GuardedFixture.posts = [], []
    GuardedFixture.reveal_receipt = False
    expected = GuardedFixture.pending["approvals"][0]["guarded_review"]["expected"]
    original_receipt = GuardedFixture.decided["approvals"][0]["decision_receipt"]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), GuardedFixture)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    checked = []
    observed_capabilities = {}
    brain_id = uid()

    def disposition(backend, request):
        return backend.call("maestro_operation_get", goal_id=request["goal_id"], operation_id=request["operation_id"])

    def retained_files():
        return {str(path.relative_to(root)): digest(path)
                for directory in (root / "brain", root / "runtime")
                for path in sorted(directory.rglob("*")) if path.is_file()}

    def old_reader_refuses():
        before = retained_files()
        process = subprocess.run([str(predecessor), "brain", "--brain-id", brain_id,
            "--listen", "127.0.0.1:0", "--records-dir", "records", "--managed-brain",
            "--vault", str(root / "brain"), "--operational-dir", str(root / "runtime")],
            capture_output=True, timeout=10)
        assert process.returncode != 0, "old binary accepted guarded enrollment"
        assert b"Maestro link enrollment identity mismatch" in process.stderr, process.stderr.decode()
        assert retained_files() == before, "old binary changed retained state"

    try:
        positive_control(server, GuardedFixture)
        checked.append("non_get_trace_positive_control")
        config = {"actor": "guarded compatibility fixture", "chat": None, "todoist": None, "t3": None,
                  "maestro": {"base_url": f"http://127.0.0.1:{server.server_port}", "instance_id": uid(), "token_env": None, "ui_origin": None}}
        with contextlib.closing(Backend(enabled, root, brain_id)) as backend:
            goal = backend.create_goal()
            backend.call("connectors_save", config=config)
            caps = backend.call("capabilities")
            observed_capabilities["enabled"] = guarded_capabilities(caps, approval_send=True)
            checked.append("actual_configured_enabled_capabilities")
            negative_control = capability_negative_control(caps)
            checked.append("incorrect_control_capability_fixture_rejected")
            choice = backend.call("maestro_discover")["projects"][0]
            issue = choice["issues"][0]
            link_request = {"operation_id": uid(), "goal_id": goal,
                "selection_guard": issue["selection_guard"], "project_id": choice["project_id"],
                "project_name": choice["name"], "repo": choice["repo"], "issue_number": issue["issue"]["number"]}
            linked = backend.call("maestro_link", **link_request)
        # A working old reader is the positive control for the later refusal.
        # It must accept this same brain before guarded enrollment is added.
        with contextlib.closing(Backend(predecessor, root, brain_id)) as legacy:
            assert legacy.call("maestro_get", goal_id=goal)["link"]["id"] == linked["link_id"]
            link_operation = legacy.call("maestro_operation_get", goal_id=goal, operation_id=link_request["operation_id"])
            assert link_operation["status"] == "committed" and link_operation["receipt"] == linked
            assert legacy.call("maestro_link", **link_request) == linked
        checked.append("old_binary_positive_boot_and_read_before_guarded_enrollment")
        with contextlib.closing(Backend(enabled, root, brain_id)) as backend:
            review_result = backend.call("maestro_approval_review", goal_id=goal,
                expected_link_id=linked["link_id"], approval_id=expected["approval_id"])
            assert review_result["goal_id"] == goal and review_result["link_id"] == linked["link_id"]
            view = review_result["view"]
            assert view["supported"] and view["review"]["expected"] == expected
            request = {"operation_id": uid(), "goal_id": goal, "expected_link_id": linked["link_id"],
                "instance": {key: config["maestro"][key] for key in ("base_url", "instance_id")},
                "review": view["review"], "decision": "approved", "actor": "compatibility operator", "reason": "Retained client reason"}
            reply = backend.request("maestro_approval_decision", **request)
            assert not reply["ok"], reply
            pending = disposition(backend, request)
            assert pending["status"] == "pending" and pending["request"] == request, pending
            assert len(GuardedFixture.posts) == 1, GuardedFixture.posts
            expected_path = "/api/v1/fleet/approvals/" + urllib.parse.quote(expected["approval_id"], safe="") + "/approve?" + urllib.parse.urlencode({"project": expected["project_name"]})
            assert GuardedFixture.posts[0]["path"] == expected_path, GuardedFixture.posts
            assert GuardedFixture.posts[0]["body"] == {"actor": request["actor"], "reason": request["reason"], "expected": expected}
            marker = json.loads((root / "runtime/maestro-links-enrollment.json").read_text())
            assert marker["maestro_approval_decisions"] == 1 and marker["maestro_operation_dispositions"] == 1
            goal_path = root / "brain" / backend.call("snapshot", goal_id=goal)["source_paths"]["goal"]
            goal_sha = digest(goal_path)
        checked.append("actual_go_review_single_post_lost_reply_retains_exact_pending")
        old_reader_refuses()
        checked.append("old_binary_refuses_pending_before_mutation")
        posts_before = len(GuardedFixture.posts)
        trace_boundary = len(GuardedFixture.methods)
        with contextlib.closing(Backend(maintenance, root, brain_id)) as backend:
            caps = backend.call("capabilities")
            observed_capabilities["maintenance"] = guarded_capabilities(caps, approval_send=False)
            checked.append("actual_configured_maintenance_capabilities")
            assert disposition(backend, request) == pending
            retry = backend.request("maestro_approval_decision", **request)
            assert not retry["ok"] and disposition(backend, request) == pending, retry
            fresh = {**request, "operation_id": uid()}
            assert not backend.request("maestro_approval_decision", **fresh)["ok"]
            assert len(GuardedFixture.posts) == posts_before
            lookup = {"goal_id": goal, "operation_id": request["operation_id"]}
            unresolved = backend.call("maestro_approval_reconcile", **lookup)
            assert unresolved["status"] == "pending" and disposition(backend, request) == pending
            checked.append("maintenance_restart_preserves_pending_and_disables_new_and_retry_posts")
            backend.call("maestro_unlink", operation_id=uid(), goal_id=goal, expected_link_id=linked["link_id"])
            GuardedFixture.reveal_receipt = True
            receipt = backend.call("maestro_approval_reconcile", **lookup)
            assert receipt["decision_receipt"] == original_receipt, receipt
            assert receipt["execution_status"] == "execution_skipped"
            committed = disposition(backend, request)
            assert committed["status"] == "committed" and committed["request"] == request
            assert backend.call("maestro_get", goal_id=goal)["link"] is None
            assert backend.call("snapshot", goal_id=goal)["stages"] == [] and digest(goal_path) == goal_sha
        checked.append("get_only_receipt_reconciliation_after_unlink_without_goal_completion")
        old_reader_refuses()
        checked.append("old_binary_refuses_committed_receipt_before_mutation")
        server.shutdown()
        server.server_close()
        thread.join(3)
        with contextlib.closing(Backend(maintenance, root, brain_id)) as backend:
            assert disposition(backend, request) == committed
            assert backend.call("maestro_approval_decision", **request) == receipt
            assert backend.call("maestro_approval_reconcile", **lookup) == receipt
            assert backend.call("maestro_get", goal_id=goal)["link"] is None
            assert digest(goal_path) == goal_sha
        checked.append("offline_maintenance_restart_preserves_exact_receipt_replay")
        assert len(GuardedFixture.posts) == posts_before == 1
        maintenance_methods = GuardedFixture.methods[trace_boundary:]
        assert maintenance_methods and all(method == "GET" and path == "/api/v1/fleet" for method, path in maintenance_methods)
        assert binary_hashes == {"enabled": digest(enabled), "maintenance": digest(maintenance), "predecessor": digest(predecessor)}, "binary changed during verification"
        result = {"schema": "okilum-maestro-guarded-maintenance/v1", "passed": True,
            "enabled_sha256": binary_hashes["enabled"], "maintenance_sha256": binary_hashes["maintenance"],
            "predecessor_sha256": binary_hashes["predecessor"], "wire_fixture_provenance_sha256": digest(wire_fixture / "provenance.json"),
            "enabled_post_count": 1, "maintenance_post_count": 0, "methods": GuardedFixture.methods,
            "observed_capabilities": observed_capabilities,
            "capability_negative_control": negative_control,
            "verified": checked, "live_enrollment": False}
        (root / "verification.json").write_text(json.dumps(result, indent=2) + "\n")
        return result
    finally:
        server.shutdown()
        server.server_close()
        thread.join(3)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--enabled", required=True, type=pathlib.Path)
    parser.add_argument("--maintenance", required=True, type=pathlib.Path)
    parser.add_argument("--guarded", action="store_true")
    parser.add_argument("--predecessor", type=pathlib.Path)
    parser.add_argument("--wire-fixture", type=pathlib.Path)
    parser.add_argument("--fixture-root", type=pathlib.Path)
    args = parser.parse_args()
    if args.guarded:
        if not (args.predecessor and args.wire_fixture and args.fixture_root):
            parser.error("--guarded requires --predecessor, --wire-fixture and --fixture-root")
        result = verify_guarded(args.enabled.resolve(), args.maintenance.resolve(),
            args.predecessor.resolve(), args.wire_fixture.resolve(), args.fixture_root.resolve())
    else:
        result = verify(args.enabled.resolve(), args.maintenance.resolve())
    print(json.dumps(result, indent=2))
