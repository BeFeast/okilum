#!/usr/bin/env python3
"""Recover an enabled writer's actual failed Markdown projection in maintenance."""
import base64
import hashlib
import contextlib
import importlib.util
import json
import pathlib
import tempfile
import threading

spec = importlib.util.spec_from_file_location("maintenance", pathlib.Path(__file__).with_name("verify-maestro-maintenance.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)


def verify(enabled, maintenance):
    base.Fixture.methods = []
    base.Fixture.generation = 1
    server = base.http.server.ThreadingHTTPServer(("127.0.0.1", 0), base.Fixture)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        base.positive_control(server, base.Fixture)
        with tempfile.TemporaryDirectory(prefix="tessera-maintenance-source-") as temporary:
            root = pathlib.Path(temporary)
            records = root / "brain/records"
            records.mkdir(parents=True)
            (root / "runtime").mkdir()
            brain = base.uid()
            with contextlib.closing(base.Backend(enabled, root, brain)) as backend:
                goal = backend.create_goal()
                goal_path = root / "brain" / backend.call("snapshot", goal_id=goal)["source_paths"]["goal"]
                goal_sha = base.digest(goal_path)
                config = {"actor": "projection fixture", "chat": None, "todoist": None, "t3": None, "maestro": {"base_url": f"http://127.0.0.1:{server.server_port}", "instance_id": base.uid(), "token_env": None, "ui_origin": None}}
                backend.call("connectors_save", config=config)
                choice = backend.call("maestro_discover")["projects"][0]
                request = {"operation_id": base.uid(), "goal_id": goal, "selection_guard": choice["issues"][0]["selection_guard"], "project_id": choice["project_id"], "project_name": choice["name"], "repo": choice["repo"], "issue_number": 42}
                records.chmod(0o555)
                try:
                    # Fail if this host bypasses the intended permission fault (e.g. root).
                    try:
                        (records / "positive-control.tmp").write_bytes(b"must fail")
                    except PermissionError:
                        pass
                    else:
                        raise AssertionError("Permission fault not effective; run the artifact matrix without DAC override")
                    failed = backend.request("maestro_link", **request)
                    assert not failed["ok"] and failed["error"]["code"] == "maestro_operation_committed", failed
                    disposition = failed["error"]["maestro_operation"]
                    assert disposition["status"] == "committed" and disposition["request"] == request
                    receipt = disposition["receipt"]
                    state = json.loads((root / "runtime/state.json").read_text())
                    assert state["pending_writes"], "positive control: genuine source projection must remain pending"
                    pending = state["pending_writes"]
                    expected = []
                    for write in pending:
                        content = base64.b64decode(write["content_base64"], validate=True)
                        expected.append((write, content, "sha256:" + hashlib.sha256(content).hexdigest()))
                    assert base.digest(goal_path) == goal_sha
                finally:
                    records.chmod(0o755)
            with contextlib.closing(base.Backend(maintenance, root, brain)) as backend:
                assert backend.call("capabilities")["maestro_link"] is False
                assert backend.call("maestro_link", **request) == receipt
                assert not json.loads((root / "runtime/state.json").read_text())["pending_writes"]
                for write, content, revision in expected:
                    assert (root / "brain" / write["path"]).read_bytes() == content
                    record = json.loads((root / "runtime/source" / (write["operation_id"] + ".json")).read_text())
                    assert record["request"] == write
                    retained = record["receipt"]
                    assert retained["operation_id"] == write["operation_id"]
                    assert retained["path"] == write["path"]
                    assert retained["revision"] == revision
                    assert retained["previous_revision"] == write["expected_revision"]
                    assert retained["outcome"] == "written"
                current = backend.call("maestro_get", goal_id=goal)
                assert current["link"]["id"] == receipt["link_id"]
                assert len(json.loads((root / "runtime/state.json").read_text())["maestro_journal"]["links"]) == 1
                paths = [current["source_paths"]["link"], *current["source_paths"]["observations"].values()]
                for path in paths:
                    assert (root / "brain" / path).is_file()
                recovered = {path: base.digest(root / "brain" / path) for path in paths}
                assert base.digest(goal_path) == goal_sha and backend.call("snapshot", goal_id=goal)["stages"] == []
            with contextlib.closing(base.Backend(maintenance, root, brain)) as backend:
                assert backend.call("maestro_link", **request) == receipt
                assert {path: base.digest(root / "brain" / path) for path in paths} == recovered
                assert base.digest(goal_path) == goal_sha
            assert base.Fixture.methods and all(method == "GET" for method, _ in base.Fixture.methods)
            return {"passed": True, "enabled_sha256": base.digest(enabled), "maintenance_sha256": base.digest(maintenance), "checks": ["permission_failure_positive_control", "actual_committed_operation_pending_projection", "maintenance_flushes_exact_original_write_bytes_and_source_receipt", "exact_original_link_receipt_replayed", "canonical_projection_survives_second_restart", "goal_unchanged_no_stage", "non_get_positive_control"]}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(3)
