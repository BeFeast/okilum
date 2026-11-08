# Private feedback identity (#2683)

Feedback keeps one random UUID v4 in the app configuration directory's
`okilum/feedback-identity.json`, separately from saved feedback drafts and
rebuildable indexes. A shared file lock serializes first creation and explicit
name changes across app processes. Invalid state is retained and stops Send;
opening or clearing a name never rotates the installation identity.

The optional **Name or GitHub** field is remembered locally. Sending includes
the UUID as the separate multipart `install_id` field and includes the separate
`reporter_name` field only when nonempty. Neither belongs to public system
metadata, the Copy report action or debug output. The form explains which data
are public and which identity fields are sent privately. Names are limited to
128 characters without control characters.

The existing pomoi feedback service accepts these fields after its compatible
server deployment ([pomoi #61](https://git.oklabs.uk/BeFeast/pomoi/issues/61)).
Legacy clients remain supported. Raw values are held only in the private
feedback ledger; no automatic expiry is claimed. The service publishes only an
opaque grouping label: `tester:t<hash8>` when a name is supplied, otherwise
`install:<hash8>`. The HMAC is domain-separated and keyed by the existing service
secret; names and UUIDs are never included in public issue bodies, metadata,
receipts or request logs. The private operator legend associates these aliases
with known reporters. A self-reported name is not an authenticated identity;
different installations are not silently merged based on matching names.

The label prefix distinguishes voluntarily named reports for triage. The
app sends no profile name, email, account identifier or host fingerprint.
Screenshots/video and message text remain explicit public attachments; logs
remain private. Users can clear the remembered name and request deletion of
the private service record using the Privacy page contact.

Acceptance needs exact-source mandatory checks, hosted Mac/Windows mounted form,
multipart and persistence controls with a compiled missing-field negative, and
deployed-service tests using a real isolated database. Final delivery requires
controlled named and anonymous reports from a containing beta, stable labels,
public payload inspection and private ledger reconciliation. Source checks and
publication do not establish installed Mac behavior; verify that on M4 with a
screenshot under the current installation authority.

## Empty reports and default diagnostics (#2744)

Ordinary feedback accepts an empty or whitespace-only message and no title or
reporter name. Send remains available while composing; transport and attachment
work still prevent duplicate submissions. Existing identity, attachment and
storage failures stay visible and preserve the draft. New reports attach the
last 250 diagnostic log lines by default on every OS; users can uncheck the
attachment and recovered drafts keep the user's saved choice. Sprint reports
retain their existing default message and diagnostic collector.

The companion pomoi service accepts absent/empty text and generates
`Feedback from <OS> <version>` for a report with neither a title nor a text
first line. System metadata and the private log attachment are preserved. The
private installation identity and optional remembered name remain unchanged.

## Fast reporting (#2749)

Description comes before the optional remembered name and receives focus when
Feedback opens. Enter in the description sends once; Cmd+Enter on macOS or
Ctrl+Enter on Linux/Windows inserts a newline. Shift+Enter also remains a newline.
These shortcuts are scoped to the description, so editing the name or attachment
limits cannot submit a report. Active IME composition and the 500 ms commit-settle
window consume Enter without sending or altering the candidate. Sending and
attachment work retain the existing duplicate-submission guards.
