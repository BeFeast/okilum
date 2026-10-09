# Optional read-only Forgejo aggregation (#510 PR 4)

The Python standard-library collector runs beside Inbox on CT119. It performs only
GET requests to one explicitly configured HTTPS Forgejo origin, with redirects and
proxy environment disabled. Use a separate account token with **read-only** user,
repository and issue access to the selected account's repositories. No T3/Maestro
credential or mutation API is used. Do not put credentials in an image or Git.

Private mode-0600 collector config:

```json
{
  "owner_id": "<Inbox owner UUID>",
  "base_url": "https://git.oklabs.uk",
  "account_id": 123,
  "credential_file": "/run/credentials/forgejo.json",
  "cache_file": "/derived/forgejo.json",
  "projects": {
    "<Inbox project UUID>": [
      {"repo_id": 456, "launch_target_ids": ["<explicitly linked target ID>"]}
    ]
  }
}
```

Credential file: `{"token":"…"}`, mode 0600. `account_id` is the numeric `/user`
identity, never the login spelling. The config (project/repository associations)
is operational configuration and must be backed up; `/derived` is replaceable.
Changing account/instance requires a new cache, never reusing another identity.
Secrets should come from Infisical `services/prod/okilum`, outside the checkout.

Run `python3 inbox/forgejo/collector.py --config /private/forgejo.json` (or
`--once`). One process owns a cache lock. Five-minute intervals; each request has
a 15-second timeout, each sync a 240-second/64 MB input budget. Every endpoint is
paginated until an empty page; account organization memberships and their public
repositories are traversed as well as `/user/repos`; duplicate pages, malformed JSON, overflow, missing
permissions and truncated traversals are failures, never a successful empty list.

The cache includes all accessible repositories, **open** issues and PRs, all
published releases/assets and status attempts for each open PR's exact head SHA.
Release `target_commitish` is an upstream ref, not a verified immutable commit;
this projection does not invent a successful build from it. A failed PR build
cannot hide a published release. Assignees and explicitly linked executor runs
remain separate. Launch base SHA is not the executor's current HEAD or proof that
it authored a PR. Deeper release platform/channel and execution-result presentation
belongs to the project screen, not heuristics in this collector.

Explicitly disabled repository units are shown as disabled and are not requested;
a 404 on an enabled unit remains a failure. Discovery failures retain the whole previous snapshot; repository failures retain
that repository's last complete observation. Disappeared repositories are marked
unavailable and retained. Writes use fsync + atomic replacement. The API derives
staleness after 15 minutes even if the collector has stopped. No token, source issue
body, request header or raw remote exception is written to the cache or logs.

Backend: supply `serve --forgejo-cache-file /derived/forgejo.json` and mount the
same derived volume read-only. The authenticated same-origin API
`GET /api/v1/forgejo[?project=<UUID>]` filters only explicit project associations
and attaches matching launch identities; it has no write route. Without the option,
aggregation is disabled. A missing/corrupt/foreign-owner file returns unavailable.
The web panel is a read-only overview; links remain on the configured Forgejo origin.

Compose opt-in example (merge into the external operator override; preserve the
existing backend command/credentials when adding `--forgejo-cache-file`):

```yaml
services:
  forgejo:
    image: okilum-inbox-qa:local
    entrypoint: [python3, /usr/local/bin/forgejo-collector]
    command: [--config, /run/credentials/forgejo-config.json]
    restart: unless-stopped
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    volumes:
      - /opt/okilum-inbox/secrets/forgejo-config.json:/run/credentials/forgejo-config.json:ro
      - /opt/okilum-inbox/secrets/forgejo-token.json:/run/credentials/forgejo.json:ro
      - forgejo-derived:/derived
  inbox:
    volumes:
      - forgejo-derived:/derived:ro
volumes:
  forgejo-derived:
```

Initialize the derived volume with owner 1000 and mode 0700 before starting the
collector. No real vault or public route is involved. Before enabling live reads,
provision the scoped credential; fixture tests are not a live connectivity claim.

Tests: `python3 -m unittest discover -s inbox/forgejo -p 'test_*.py' -v`.
