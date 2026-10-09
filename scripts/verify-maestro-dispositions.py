#!/usr/bin/env python3
"""Execute durable operation recovery against isolated enabled/maintenance binaries.

An optional successful-receipt-only predecessor must refuse the enrolled journal
before mutation. All provider activity uses one local GET-only fixture.
"""
import argparse
import concurrent.futures
import contextlib
import importlib.util
import json
import pathlib
import subprocess
import tempfile
import threading

spec = importlib.util.spec_from_file_location("maintenance", pathlib.Path(__file__).with_name("verify-maestro-maintenance.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)


class Fixture(base.Fixture):
    entered = threading.Event()
    release = threading.Event()
    hold = False
    fail = False
    received = 0

    def do_GET(self):
        assert self.path == "/api/v1/fleet"
        type(self).received += 1
        if self.hold:
            self.entered.set()
            assert self.release.wait(6), "held GET was not released"
        if self.fail:
            self.send_error(503)
        else:
            super().do_GET()


def disposition(backend, request):
    return backend.call("maestro_operation_get", goal_id=request["goal_id"], operation_id=request["operation_id"])


def abandon(backend, request, kind="link"):
    return backend.call("maestro_operation_abandon", kind=kind, request=request)


def reject(backend, request, code):
    reply = backend.request("maestro_link", **request)
    assert reply["ok"] is False, reply
    current = reply["error"]["maestro_operation"]
    assert current["status"] == "rejected" and current["rejection"]["code"] == code, reply
    assert current["request"] == request, reply
    return current


def verify(enabled, maintenance, predecessor=None):
    Fixture.methods = []
    Fixture.received = 0
    Fixture.hold = Fixture.fail = False
    Fixture.entered.clear()
    Fixture.release.clear()
    server = base.http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    checked = []
    try:
        base.positive_control(server, Fixture)
        checked.append("non_get_detection_positive_control")
        with tempfile.TemporaryDirectory(prefix="okilum-maestro-dispositions-") as temporary:
            root = pathlib.Path(temporary)
            (root / "brain/records").mkdir(parents=True)
            (root / "runtime").mkdir()
            brain_id = base.uid()
            config = {"actor": "disposition fixture", "chat": None, "todoist": None, "t3": None,
                      "maestro": {"base_url": f"http://127.0.0.1:{server.server_port}", "instance_id": base.uid(), "token_env": None, "ui_origin": None}}
            with contextlib.closing(base.Backend(enabled, root, brain_id)) as backend:
                goal = backend.create_goal()
                backend.call("connectors_save", config=config)
                choice = backend.call("maestro_discover")["projects"][0]
                request = {"operation_id": base.uid(), "goal_id": goal,
                           "selection_guard": choice["issues"][0]["selection_guard"],
                           "project_id": choice["project_id"], "project_name": choice["name"],
                           "repo": choice["repo"], "issue_number": 42}
                unknown = disposition(backend, request)
                assert unknown["status"] == "unknown" and unknown["request"] is None
                # Positive control: a real GET is held after durable intent exists.
                Fixture.hold = True
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(backend.request, "maestro_link", **request)
                    try:
                        assert Fixture.entered.wait(4), "link GET never reached provider"
                        pending = disposition(backend, request)
                        assert pending["status"] == "pending" and pending["request"] == request
                        rejected = abandon(backend, request)
                        assert rejected["status"] == "rejected" and rejected["rejection"]["code"] == "abandoned"
                    finally:
                        Fixture.hold = False
                        Fixture.release.set()
                    reply = future.result(5)
                    assert reply["ok"] is False and reply["error"]["maestro_operation"] == rejected, reply
                assert backend.call("maestro_get", goal_id=goal)["history"] == []
                checked.append("held_get_pending_abandon_late_response_no_link")
                late = {**request, "operation_id": base.uid()}
                before_get = Fixture.received
                tombstone = abandon(backend, late)
                assert disposition(backend, late) == tombstone
                assert reject(backend, late, "abandoned") == tombstone
                assert Fixture.received == before_get, "delayed first request performed GET"
                checked.append("unknown_tombstone_delayed_first_request_no_get")
                changed = {**late, "issue_number": 43}
                assert backend.request("maestro_operation_abandon", kind="link", request=changed)["ok"] is False
                assert disposition(backend, late) == tombstone
                checked.append("changed_input_preserves_original_disposition")
                pending_request = {**request, "operation_id": base.uid()}
                Fixture.fail = True
                failure = backend.request("maestro_link", **pending_request)
                Fixture.fail = False
                assert failure["ok"] is False and failure["error"]["maestro_operation"]["status"] == "pending", failure
                checked.append("network_failure_retains_pending")
            if predecessor:
                journal = root / "runtime/state.json"
                marker = root / "runtime/maestro-links-enrollment.json"
                before_journal, before_marker = journal.read_bytes(), marker.read_bytes()
                def retained_files():
                    return {str(path.relative_to(root)): base.digest(path)
                            for directory in (root / "brain", root / "runtime")
                            for path in sorted(directory.rglob("*")) if path.is_file()}
                before_files = retained_files()
                process = subprocess.run([str(predecessor), "brain", "--brain-id", brain_id,
                    "--listen", "127.0.0.1:0", "--records-dir", "records", "--managed-brain",
                    "--vault", str(root / "brain"), "--operational-dir", str(root / "runtime")],
                    capture_output=True, timeout=10)
                assert process.returncode != 0, "old reader accepted disposition journal"
                assert b"Maestro link enrollment identity mismatch" in process.stderr, process.stderr.decode()
                assert journal.read_bytes() == before_journal and marker.read_bytes() == before_marker
                assert retained_files() == before_files, "old reader changed canonical/source/runtime files"
                checked.append("actual_predecessor_refuses_downgrade_before_journal_and_source_mutation")
            with contextlib.closing(base.Backend(maintenance, root, brain_id)) as backend:
                assert disposition(backend, pending_request)["status"] == "pending"
                assert reject(backend, request, "abandoned") == rejected
                before_get = Fixture.received
                assert reject(backend, late, "abandoned") == tombstone
                refused = reject(backend, pending_request, "new_links_disabled")
                assert Fixture.received == before_get
                maintenance_late = {**request, "operation_id": base.uid()}
                maintenance_tombstone = abandon(backend, maintenance_late)
                assert reject(backend, maintenance_late, "abandoned") == maintenance_tombstone
                checked.append("maintenance_pending_restart_and_unknown_tombstone")
            with contextlib.closing(base.Backend(enabled, root, brain_id)) as backend:
                before_get = Fixture.received
                assert reject(backend, pending_request, "new_links_disabled") == refused
                assert reject(backend, maintenance_late, "abandoned") == maintenance_tombstone
                assert Fixture.received == before_get
                checked.append("rejected_survives_enabled_restart")
                success_request = {**request, "operation_id": base.uid()}
                linked = backend.call("maestro_link", **success_request)
                committed = abandon(backend, success_request)
                assert committed["status"] == "committed" and committed["receipt"] == linked
                assert backend.call("maestro_link", **success_request) == linked
                unlink_request = {"operation_id": base.uid(), "goal_id": goal, "expected_link_id": linked["link_id"]}
                wrong_kind = {**unlink_request, "operation_id": success_request["operation_id"]}
                assert backend.request("maestro_operation_abandon", kind="unlink", request=wrong_kind)["ok"] is False
                unlinked = backend.call("maestro_unlink", **unlink_request)
                assert abandon(backend, unlink_request, "unlink")["receipt"] == unlinked
                stale = {**unlink_request, "operation_id": base.uid()}
                stale_reply = backend.request("maestro_unlink", **stale)
                assert stale_reply["error"]["maestro_operation"]["status"] == "rejected"
                checked.append("committed_link_and_unlink_win_abandonment")
            server.shutdown()
            server.server_close()
            thread.join(3)
            with contextlib.closing(base.Backend(maintenance, root, brain_id)) as backend:
                assert backend.call("maestro_link", **success_request) == linked
                assert backend.call("maestro_unlink", **unlink_request) == unlinked
                assert disposition(backend, stale)["status"] == "rejected"
                assert abandon(backend, late) == tombstone
                assert backend.call("maestro_get", goal_id=goal)["link"] is None
                assert backend.call("snapshot", goal_id=goal)["stages"] == []
                checked.append("offline_query_abandon_and_exact_receipts_after_restart")
            assert Fixture.methods and all(m == "GET" and p == "/api/v1/fleet" for m, p in Fixture.methods)
            return {"schema": "okilum-maestro-dispositions-verification/v1", "passed": True,
                    "enabled_sha256": base.digest(enabled), "maintenance_sha256": base.digest(maintenance),
                    "predecessor_sha256": base.digest(predecessor) if predecessor else None,
                    "fixture_requests": Fixture.received, "remote_mutations": 0, "verified": checked}
    finally:
        Fixture.release.set()
        server.shutdown()
        server.server_close()
        thread.join(3)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--enabled", required=True, type=pathlib.Path)
    parser.add_argument("--maintenance", required=True, type=pathlib.Path)
    parser.add_argument("--predecessor", type=pathlib.Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.enabled.resolve(), args.maintenance.resolve(), args.predecessor.resolve() if args.predecessor else None), indent=2))
