#!/usr/bin/env python3
"""Loopback T3 image acceptance fixture; never invokes a model or opens a network.

Set OKILUM_IMAGE_QA_DIR to an isolated evidence directory with a project/ folder,
then configure this executable as the isolated T3 Codex binaryPath.
Receipts contain only synthetic turn inputs, image SHA256 and byte counts.
Provider configuration (including credentials) is never logged.
"""

import sys, json, os, uuid, pathlib, hashlib, base64

q = pathlib.Path(os.environ["OKILUM_IMAGE_QA_DIR"]).resolve(strict=True)
assert (q / "project").is_dir(), "Use an isolated QA directory with project/"
if "--version" in sys.argv:
    print("codex-cli 0.135.0")
    sys.exit()


def send(x):
    print(json.dumps(x), flush=True)


def note(method, params):
    send(dict(method=method, params=params))


def thread(tid):
    return dict(
        id=tid,
        projectId=None,
        sessionId=tid,
        preview="",
        ephemeral=False,
        modelProvider="openai",
        createdAt=1760000000,
        updatedAt=1760000000,
        path=None,
        cwd=str(q / "project"),
        cliVersion="0.135.0",
        source="appServer",
        gitInfo=None,
        name=None,
        turns=[],
        status=dict(type="idle"),
    )


for line in sys.stdin:
    try:
        r = json.loads(line)
        m = r.get("method")
        p = r.get("params") or {}
        rid = r.get("id")
        with (q / "provider-rpc.jsonl").open("a") as f:
            f.write(json.dumps(dict(method=m, id=rid)) + "\n")
        if rid is None:
            continue
        if m == "initialize":
            result = dict(
                userAgent="okilum-fixture",
                codexHome=str(q / "home"),
                platformFamily="unix",
                platformOs="linux",
            )
        elif m == "account/read":
            result = dict(
                account=dict(
                    type="chatgpt", email="fixture@example.invalid", planType="plus"
                ),
                requiresOpenaiAuth=False,
            )
        elif m == "model/list":
            result = dict(
                data=[
                    dict(
                        id="gpt-6.1-sol",
                        model="gpt-6.1-sol",
                        displayName="Fixture image receiver",
                        description="No network",
                        hidden=False,
                        isDefault=True,
                        supportedReasoningEfforts=[
                            dict(reasoningEffort="high", description="Fixture")
                        ],
                        defaultReasoningEffort="high",
                        inputModalities=["text", "image"],
                        supportsPersonality=False,
                    )
                ],
                nextCursor=None,
            )
        elif m in ("thread/start", "thread/resume"):
            tid = p.get("threadId") or "fixture-" + str(uuid.uuid4())
            result = dict(
                thread=thread(tid),
                model="gpt-6.1-sol",
                modelProvider="openai",
                cwd=str(q / "project"),
                approvalPolicy="never",
                approvalsReviewer="user",
                sandbox=dict(type="dangerFullAccess"),
                reasoningEffort="high",
            )
        elif m == "turn/start":
            turn = "fixture-turn-" + str(uuid.uuid4())
            tid = p["threadId"]
            images = []
            for item in p.get("input", []):
                if item["type"] == "localImage":
                    b = pathlib.Path(item["path"]).read_bytes()
                elif item["type"] == "image":
                    assert item["url"].startswith("data:image/"), (
                        "Fixture accepts inline image bytes only"
                    )
                    b = base64.b64decode(item["url"].split(",", 1)[1], validate=True)
                else:
                    continue
                sha = hashlib.sha256(b).hexdigest()
                (q / ("provider-" + sha + ".bin")).write_bytes(b)
                images.append(dict(size=len(b), sha256=sha))
            with (q / "provider-receipts.jsonl").open("a") as f:
                f.write(
                    json.dumps(
                        dict(
                            threadId=tid,
                            turnId=turn,
                            input=p.get("input"),
                            images=images,
                        )
                    )
                    + "\n"
                )
            state = dict(id=turn, items=[], status="inProgress", error=None)
            send(dict(id=rid, result=dict(turn=state)))
            note("turn/started", dict(threadId=tid, turn=state))
            item = dict(
                id="fixture-answer-" + turn,
                type="agentMessage",
                text="Fixture received " + str(len(images)) + " image(s).",
            )
            note("item/started", dict(threadId=tid, turnId=turn, item=item))
            note(
                "item/agentMessage/delta",
                dict(threadId=tid, turnId=turn, itemId=item["id"], delta=item["text"]),
            )
            note("item/completed", dict(threadId=tid, turnId=turn, item=item))
            note(
                "turn/completed",
                dict(
                    threadId=tid,
                    turn=dict(id=turn, items=[item], status="completed", error=None),
                ),
            )
            continue
        elif m == "skills/list":
            result = dict(data=[dict(cwd=str(q / "project"), skills=[], errors=[])])
        elif m == "config/read":
            result = dict(config={}, origins={}, layers=[])
        elif m == "thread/read":
            result = dict(thread=thread(p["threadId"]))
        else:
            result = {}
        send(dict(id=rid, result=result))
    except Exception as e:
        with (q / "provider-errors.log").open("a") as f:
            f.write(repr(e) + "\n")
        if "rid" in locals():
            send(dict(id=rid, error=dict(code=-32603, message=str(e))))
