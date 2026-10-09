#!/usr/bin/env python3
"""Actual frozen-binary reviewed-packet compatibility; isolated files, no providers."""
import argparse
import base64
import contextlib
import importlib.util
import io
import json
import pathlib
import tarfile
import tempfile

spec = importlib.util.spec_from_file_location("maintenance", pathlib.Path(__file__).with_name("verify-maestro-maintenance.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
OLD5B_SHA256 = "5b129b192156585bd3fbf3aa48f4bc5c8f5c726a8df782edfbad1b78e7c47bd9"
MISMATCH = "goal definition changed; prepare and review a new context"


def files(root):
    return {str(p.relative_to(root)): base.digest(p) for p in root.rglob("*") if p.is_file()}


def verify(writer, reader, negative=False, citation_provider=None):
    if negative:
        assert base.digest(reader) == OLD5B_SHA256, "negative control requires the retained old5b artifact"
    with tempfile.TemporaryDirectory(prefix="okilum-packet-matrix-") as directory:
        root = pathlib.Path(directory)
        (root / "brain/records").mkdir(parents=True)
        (root / "runtime").mkdir()
        brain, goal, decision = base.uid(), base.uid(), base.uid()
        with contextlib.closing(base.Backend(citation_provider or writer, root, brain)) as backend:
            backend.call("create_goal", goal={"id": goal, "title": "Exact packet compatibility", "status": "draft", "criteria": [{"id": "C1", "description": "Observe real behavior", "requires_human": True}], "stage_ids": [], "task_ref": None}, body="# Fixture\n")
            metadata = {"schema": "ai-brain/v1", "brain_id": brain, "record_type": "decision", "id": decision, "goal_id": goal, "stage_id": None, "attention_id": "fixture-attention", "attention_revision": "fixture-revision", "received_at": "2026-09-07T04:00:00Z", "actor_id": "fixture operator", "verification": "unverified", "source": {"channel": "native", "instance_id": base.uid(), "account_id": "local", "actor_id": "fixture operator", "chat_id": None, "topic_id": None, "message_id": base.uid(), "update_id": base.uid(), "uri": None}}
            path = "records/decision-" + decision + ".md"
            source = "---\n" + json.dumps(metadata) + "\n---\n# Saved reply\nPreserve provenance.\n"
            (root / "brain" / path).write_text(source)
            citation = backend.call("goal_context_brief", goal_id=goal)["inputs"][0]["citation"]
            assert citation["excerpt"] == source
        with contextlib.closing(base.Backend(writer, root, brain)) as backend:
            packet = backend.call("context_prepare", goal_id=goal, query="Manual selected context", scope={"goal_id": goal, "mode": "goal"}, citations=[citation], pinned_citation_ids=[citation["citation_id"]])["packet"]
            packet = backend.call("context_revise", goal_id=goal, packet_id=packet["id"], expected_revision=packet["revision"], text="Exact manual guidance\nPreserve this review.")["packet"]
            assert packet["reviewed"] and not packet["stale"], "positive control: writer must establish a reviewed packet"
            packet_path = "records/reviewed-context-" + packet["id"] + ".md"
        original = files(root / "brain")
        for restart in range(2):
            with contextlib.closing(base.Backend(reader, root, brain)) as backend:
                incoming = backend.call("context_get", goal_id=goal, packet_id=packet["id"])["packet"]
                assert files(root / "brain") == original, "reader changed canonical source/goal/packet"
                for field in ["id", "revision", "text", "citations", "pinned_citation_ids", "goal_definition_sha256", "reviewed_content_sha256"]:
                    assert field in packet and incoming[field] == packet[field], (field, incoming)
                if negative:
                    assert incoming["reviewed"] is False and incoming["stale"] is True, incoming
                    assert incoming["stale_reason"] == MISMATCH, incoming
                else:
                    assert incoming["reviewed"] and not incoming["stale"], incoming
                export = backend.call("export_prepare")
                data = bytearray()
                while len(data) < export["bytes"]:
                    chunk = backend.call("export_chunk", export_id=export["export_id"], offset=len(data))
                    content = base64.b64decode(chunk["content_base64"], validate=True)
                    assert content, "export stopped advancing"
                    data.extend(content)
                backend.call("export_release", export_id=export["export_id"])
                with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                    for canonical in [path, packet_path, "records/goal-" + goal + ".md"]:
                        assert archive.extractfile("brain/" + canonical).read() == (root / "brain" / canonical).read_bytes()
                assert files(root / "brain") == original
                if restart == 1 and not negative:
                    (root / "brain" / path).write_text(source.replace("Preserve provenance.", "Changed source."))
                    stale = backend.call("context_get", goal_id=goal, packet_id=packet["id"])["packet"]
                    assert stale["stale"] and stale["stale_reason"] == "citation source revision changed", stale
                    rejected = backend.request("context_revise", goal_id=goal, packet_id=packet["id"], expected_revision=packet["revision"], text=packet["text"])
                    assert not rejected["ok"], rejected
                    assert base.digest(root / "brain" / packet_path) == original[packet_path]
        return {"passed": True, "writer_sha256": base.digest(writer), "reader_sha256": base.digest(reader), "negative_old5b": negative, "checks": ["writer_prepared_and_explicitly_reviewed", "unchanged_source_goal_guidance_citations_pins_hashes", "reader_restart", "exact_export", "specific_old5b_goal_definition_mismatch" if negative else "real_source_edit_stale_review_rejected"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--writer", required=True, type=pathlib.Path)
    parser.add_argument("--reader", required=True, type=pathlib.Path)
    parser.add_argument("--negative-old5b", action="store_true")
    parser.add_argument("--citation-provider", type=pathlib.Path, help="Seed exact citation when the writer predates goal_context_brief; writer still prepares/reviews the packet")
    args = parser.parse_args()
    print(json.dumps(verify(args.writer.resolve(), args.reader.resolve(), args.negative_old5b, args.citation_provider), indent=2))
