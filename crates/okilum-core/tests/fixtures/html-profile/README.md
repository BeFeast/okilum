# Note HTML profile 1

Use `vault/` as the fixture vault. Each document under `expected/` is the
complete HTML for the Markdown file at the same relative path in `vault/`.

```sh
okilum-cored render --profile mobile nested/Note.md --vault crates/okilum-core/tests/fixtures/html-profile/vault
```

The profile marker is `<meta name="html_profile" content="1">`. A successful
note link uses `okilum-note:<path>#<anchor>`, with the fragment omitted for
note-top navigation. Links to attachments and local images use
`okilum-asset:<path>`. Percent-decode vault-relative paths once, after
splitting the fragment. Forward fragments as emitted to the destination
document. Heading IDs use the same case and whitespace matching as the
desktop reader. Missing, ambiguous and unsupported links use inert spans
with `data-link-status` and preserve their visible labels.

HTTP and HTTPS destinations retain their authored scheme. The document CSP
allows image loads via `okilum-asset:`. The host controls external navigation,
asset authorization and revision mapping.
