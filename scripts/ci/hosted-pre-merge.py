#!/usr/bin/env python3
"""Run the existing fast phases on a hosted runner, retaining strict receipts."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def main(root=ROOT):
    if (sys.platform != "linux" or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or not re.fullmatch(r"[0-9a-f]{40}", os.environ.get("OKILUM_SOURCE_COMMIT", ""))):
        print("Hosted fast gate requires GitHub-hosted execution and a full source SHA", file=sys.stderr)
        return 75
    directory = Path(tempfile.mkdtemp(prefix="hosted-pre-merge-", dir=os.environ["RUNNER_TEMP"]))
    receipt, shards = directory / "phases.json", directory / "execution.json"
    env = dict(os.environ, OKILUM_POOL_RECEIPT=str(receipt))
    result = subprocess.run([sys.executable, str(HERE / "test-group.py"),
                             "hosted-pre-merge", "1800", "bash",
                             str(root / "scripts/ci/pool-pre-merge.sh")], cwd=root, env=env, check=False)
    try:
        phases = json.loads(receipt.read_text())
        complete = (isinstance(phases, list) and phases
                    and all(isinstance(p, dict) for p in phases)
                    and phases[-1].get("name") == "Pool shard complete"
                    and type(phases[-1].get("code")) is int
                    and (phases[-1]["code"] == 0) == (result.returncode == 0))
    except (OSError, ValueError):
        complete = False
    if result.returncode not in (0, 1) or not complete:
        print("Hosted fast gate produced no complete phase verdict", file=sys.stderr)
        return 75
    # This is an honest hosted execution receipt, not a synthetic build-slot
    # receipt. The unchanged reporters check EVERY phase plus this completion.
    state = "pass" if result.returncode == 0 else "fail"
    shards.write_text(json.dumps([{"shard": "github-hosted", "state": state,
                                  "rc": result.returncode, "source": env["OKILUM_SOURCE_COMMIT"]}]))
    with open(os.environ["GITHUB_ENV"], "a") as output:
        output.write(f"OKILUM_POOL_RECEIPT={receipt}\nOKILUM_POOL_SHARDS={shards}\n")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write("kind=verdict\n")
    print(f"Hosted fast execution source={env['OKILUM_SOURCE_COMMIT']} state={state}", flush=True)
    return 0  # Code failures remain failures in their named reporter steps.


if __name__ == "__main__":
    raise SystemExit(main())
