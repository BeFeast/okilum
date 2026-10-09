# Ordinary project workspace

Issue #80 adds an ordinary app Workspace entrypoint, retaining the existing
read-only Reader and the backend-backed BrainView. Starting `okilum` without
arguments opens the workspace entry. The existing `--vault` path remains available;
a saved project brain reopens automatically when no local vault is requested.
An explicit vault request preserves the Reader entrypoint; the saved brain remains
available through Workspace → Retry saved brain. Surface switching retains both entities and drafts.

Select an existing loopback backend and project label in Workspace settings.
**Select this backend as project brain** explicitly adopts the identity advertised
by that backend; **Retry saved brain** requires the previous identity. No service
is installed or started by selection. Missing services show their error and Retry;
old servers lacking guarded workspace capability cannot be selected. The legacy
`--brain-endpoint` POC entrypoint is unchanged for compatibility.

## Persistence and boundaries

`$XDG_CONFIG_HOME/okilum/workspace.json` (fallback `~/.config/okilum/workspace.json`)
is an atomically replaced, non-secret JSON profile:

```json
{
  "schema": "okilum-workspace/v1",
  "label": "Project brain",
  "endpoint": "127.0.0.1:24161",
  "identity": {
    "brain_id": "01000000-0000-4000-8000-000000000001",
    "root": "/srv/brains/project",
    "records_dir": "records",
    "managed": true
  }
}
```

`root` is the canonical **backend** root, not a path to open on the desktop.
Managed note listing, source reads, links and preview use the source API and
existing Reader rendering. The profile never maps a remote root to local files.
Read-only vault selection uses a separately chosen local directory. Record
placement stays the runner's existing `records_dir`; no records are migrated.
Operational journals remain outside the canonical root and derived index.

Capabilities adds `workspace` with that identity and `workspace_guard: true`.
Guarded requests use the additive `ai-brain/workspace-v1` envelope with a required
`expected_workspace` equal to the advertised identity. Replies echo the dialect.
The backend rejects a mismatch before command dispatch, including chat. The
separate dialect also makes a legacy backend reject requests if it replaces the
endpoint after the initial handshake. The `ai-brain/v1` protocol is unchanged;
its optional guard is supported for compatible callers, but new guarded clients
use the distinct dialect for downgrade protection.

Surface switching is disabled during a pending backend connection, and completion
rechecks dirty state before persisting the selection or replacing the view.
Replacing a backend while an editable source has a dirty draft is blocked with an
explicit save/discard instruction. Switching to the retained read-only Reader or
Workspace settings preserves the draft. Dirty-draft recovery after process exit
is outside this slice. Connector configuration and richer source-editor UX are
separate alpha issues.

## Startup recovery (#317)

With no loaded Reader or Brain entity, entry uses one content column (480px maximum
for the overview, 640px for forms) and hides
unavailable workspace navigation and capture. Appearance remains available in the
header, labeled Open workspace, Workspace recovery or Workspace settings according
to entry/loaded state. Secondary action labels align left. A saved-brain failure shows the saved name, an unavailable status and
**Retry saved brain** before secondary choices. **Connection details** discloses
the attempted mode/endpoint, raw error and saved backend root; the root is a
server identity, never a local folder. No transport cause is inferred.

First launch offers **Open a project brain** and **Open local vault (read-only)**,
without a retry for a nonexistent profile. Each choice opens only its own form.
Back or Escape preserves edited field values and restores the prior overview
feedback and the selected choice button, including after mouse activation from an
unrelated keyboard focus. Stable non-tab-stop focus groups locate the retained
button without relying on the toolkit changing focus on mouse-down. Details collapse in place. Selecting another brain
explicitly adopts its advertised identity; retry freezes the saved label, endpoint
and expected identity even if the form fields have been edited or emptied.

Cold entry establishes a focus path before any mouse input, then focuses the saved
Retry action (or the first workspace choice) after enabled controls have painted.
This is one-time startup focus: a pending connection delays it, and an already
user-focused control or loaded Reader/Brain is not interrupted.

Entry simplification depends on loaded entities, not backend reachability. A
failed connection preserves existing Brain/Reader entities and leaves their
navigation reachable. Existing dirty-source and pending-connection guards remain;
there is no new offline capture, content cache, automatic retry, service management
or profile migration.

Native acceptance requires light/dark checks of recovery, details, secondary-form
return and retained Reader navigation. Unit tests verify state/identity behavior;
they do not establish native layout or installed Mac behavior.
