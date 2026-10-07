# App-owned typed-view preference storage (#636)

Persist exact-case type mappings and Tasks defaults in the existing reader-ui.json
state, outside the vault. Reuse its locked atomic writer and per-field merge.
Preserve unknown view IDs for explicit selection fallback and load old files with
default preferences. No Settings controls or native views are activated here.

Extend the independent-process regression: a newer mapping/defaults write must
survive an older process changing appearance or search case sensitivity. Conversely,
a view-preferences write must preserve a newer search setting. Check exact-case type selection and
unknown-ID fallback from the reloaded preferences. fmt, core/shell clippy, one
review and required CI before merge; no owner-vault writes.
