#!/usr/bin/env python3
"""Prepare an ordinary backend user-service definition; never install or start it."""
import argparse
import json
import pathlib
import uuid

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', type=pathlib.Path, required=True)
p.add_argument('--binary', type=pathlib.Path, required=True)
p.add_argument('--brain', type=pathlib.Path, required=True)
p.add_argument('--operational', type=pathlib.Path, required=True)
p.add_argument('--brain-id', default=None)
p.add_argument('--listen', default='127.0.0.1:24161')
a = p.parse_args()
for path in (a.binary, a.brain, a.operational):
    if not path.is_absolute():
        p.error('binary, brain and operational paths must be absolute')
if a.brain == a.operational or a.brain in a.operational.parents or a.operational in a.brain.parents:
    p.error('brain and operational directories must be disjoint')
brain_id = str(uuid.UUID(a.brain_id)) if a.brain_id else str(uuid.uuid4())
args = [str(a.binary), 'brain', '--brain-id', brain_id, '--vault', str(a.brain),
        '--operational-dir', str(a.operational), '--records-dir', 'records',
        '--listen', a.listen, '--managed-brain']
def quote(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
a.output.mkdir(parents=True, exist_ok=True)
unit = '[Unit]\nDescription=Tessera project brain backend\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart=' + ' '.join(map(quote, args)) + '\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n'
(a.output / 'tessera-brain.service').write_text(unit)
(a.output / 'backend.json').write_text(json.dumps({'brain_id': brain_id, 'brain': str(a.brain),
    'operational': str(a.operational), 'listen': a.listen, 'argv': args}, indent=2) + '\n')
(a.output / 'plan.md').write_text(f'''# Prepared ordinary Tessera backend

Preparation only: no directory migration, installation or running service change.

- Backend binary: `{a.binary}`.
- Managed brain: `{a.brain}`; records: `records/`.
- Durable operational state: `{a.operational}`.
- Brain identity: `{brain_id}`; endpoint: `{a.listen}`.
- Saved connector references: `{a.operational / 'connector-settings.json'}`.

Before an approved installation, review these exact paths and preserve an existing
brain identity/state instead of replacing it. Create the brain/records and separate
operational directories if this is a new workspace. Install the reviewed binary
and this unit under the intended user's systemd configuration, then enable/start
only this unit. Reopening ordinary Tessera selects its saved profile. A remote
desktop needs reviewed persistent loopback forwarding for backend and T3 browser
addresses; it must not use a one-off test launcher. Enabling user lingering, if
required for logout independence, belongs to that explicit installation plan.

Rollback stops/disables only this new unit and restores its previous binary/unit
if one existed. Preserve canonical Markdown and operational journals. Never rewind
external task/thread state. Credential files are existing external references;
this unit neither creates a secret store nor sources shell scripts.
''')
print(a.output)
