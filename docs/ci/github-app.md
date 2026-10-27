# GitHub bridge authentication

Forgejo remains primary. The GitHub mirror and native bridges prefer a GitHub
App installation token when all three Forgejo repository secrets are present:
`OKILUM_GITHUB_APP_ID`, `OKILUM_GITHUB_APP_INSTALLATION_ID`, and
`OKILUM_GITHUB_APP_KEY` (the PEM private key). With none set, they retain the
`OKILUM_GITHUB_MIRROR` PAT fallback, exposed to scripts as `MIRROR_TOKEN`.
Partial App configuration or App authentication failure fails closed; it never
silently uses the PAT. Installation tokens refresh five minutes before expiry
in long-running Python bridges. The bounded mirror process resolves one token.

## App setup and minimum permissions

Create the App under **BeFeast**, install it with **Only select repositories →
okilum**, and disable webhook delivery. No organization permissions or event
subscriptions are needed. Repository permissions:

| Permission | Access | Existing operation requiring it |
| --- | --- | --- |
| Metadata | Read (implicit) | Repository identity |
| Contents | Read/write | Push main, release tags and unique PR refs; delete owned PR refs; release assets |
| Workflows | Read/write | Mirrored commits and PR refs can modify `.github/workflows` |
| Actions | Read/write | Read runs/jobs/artifacts; cancel timed-out owned runs; dispatch shipping workflow |

No Pull requests, Issues, Checks, Commit statuses, or Administration permission
is required. Forgejo reports required statuses; the App does not write GitHub
checks. Token minting further restricts the installation to `okilum` and the
three write permissions above.

The owner creates the App and supplies its IDs and PEM through the secure
secret request. The CI owner installs them as Forgejo **repository** secrets
using the API, never in a checkout, PR, chat, or log. Keep the PAT during initial
activation. Verify one authorized bridge run logs `auth=App`, then its native
result. Remove all three App secrets together to restore PAT authentication.
Activation and any verification run require the manager's concrete handoff;
merging this implementation does not install or activate an App.

## Audited REST and git operations

All repository REST paths below are under `/repos/BeFeast/okilum`:

- `GET /actions/runs` (filtered by event/branch) and `GET /actions/runs/{id}`.
- `GET /actions/runs/{id}/jobs` (paginated) and
  `GET /actions/runs/{id}/attempts/1/jobs`.
- `POST /actions/runs/{id}/cancel` (existing bounded timeout cancellation).
- `GET /git/matching-refs/heads/forgejo-pr/{number}/`,
  `DELETE /git/refs/heads/{owned-ref}`, and `GET /git/ref/heads/main`.
- `POST /actions/workflows/forgejo-macos-shipping.yml/dispatches` and
  `GET /actions/workflows/forgejo-macos-shipping.yml/runs`.
- `GET /actions/workflows/forgejo-linux.yml/runs`,
  `GET /actions/runs/{id}/artifacts`, `GET /actions/artifacts/{id}`, and
  `GET /actions/artifacts/{id}/zip` for retained AppImage reuse.
- Shared release transport: `GET /releases?per_page=100&page=N`, `GET /releases/tags/{tag}`,
  `POST /releases`, `PATCH /releases/{id}`,
  `DELETE /releases/{id}`, `DELETE /releases/assets/{id}`,
  and `POST /releases/{id}/assets?name=…` on `uploads.github.com`.

The new authentication helper uses `POST
/app/installations/{installation_id}/access_tokens` with an RS256 App JWT and
`GET /rate_limit` once at bridge startup. OpenSSL signs the JWT through an
unlinked private file descriptor; Python uses only its standard library. The
key/JWT/token is never logged. The mirror child receives only the token, with
App secrets removed from its environment.

Git operations: bridges push a unique `forgejo-pr/{number}/{sha}-…` head;
cleanup deletes only matching owned refs. Mirror fetches Forgejo main and an
optional allowed release tag, checks tag ancestry, and pushes main plus that
tag. Only the rolling `beta` tag is force-updated. Tokens use askpass rather
than credential-bearing git URLs. Ref selection and publication policy stay
unchanged.

Bridges log remaining core quota once and poll with independent 30–60 second
jitter. HTTP 403/429 handling inspects remaining-quota headers and the response
body, distinguishes exhausted core quota from secondary limits and permission
errors, and never echoes arbitrary response content. Known exhausted quota is
not retried, including cleanup. Existing native-gate identity, required steps,
timeouts, fail-closed behavior and fork/local routing remain in force.
