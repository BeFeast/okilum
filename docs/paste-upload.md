# Upload of pasted and dropped files (#1073)

Settings → Editor → **Upload pasted and dropped files to pomoi.co**. Off by
default.

## Behaviour

- A paste or drop saves its images and files into the vault's attachment
  folder exactly as before. Nothing about that path changes.
- With the setting on, the upload starts only after the inserted text went
  into its note's editor: a paste, a drop, a background file copy or image
  save, a large rich-text paste (its `data:`/`file://` images) and images
  downloaded from pasted rich text. Each link that inserted text holds to a
  file the paste saved is remembered by its own place in the note (an
  `Anchor`) and exact text. A saved file the text does not link (a "keep
  original" copy) is not uploaded; a pasted note (`.md`) never is.
- When the provider confirms the upload, only those links are rewritten to
  the uploaded copy, only while each is still there unchanged, and only in
  the editor of the note the paste went into (never the note the user moved
  to, never a copy elsewhere in the note or inside a code block):
  `![name](https://pomoi.co/f/<id>/<name>)` for images, `[name](…)` for
  other files (Okilum renders a remote embed only as an image).
- The local file always stays (decision Q2, 17.11). A failed upload, a
  closed note or a link the user changed meanwhile leaves the local link,
  and a toast says which.
- The setting is checked again before every file is sent. Turning it off
  stops the job: nothing more is sent, and an upload already under way no
  longer changes the note.
- Images and other files both upload (Q3).

## Privacy and rendering (Q1)

- Uploads are sent with `private: true` and `expire: never`: links do not
  rot after pomoi's 72-hour default, and the URL alone does not open them.
- Okilum loads `https://pomoi.co/f/…` images itself, with the saved token, in
  Reader, Live Preview, link hover and imported bodies
  (`note_upload::image_source`, a GPUI custom image source). Other remote
  images keep their existing path.
- Other apps (Obsidian, GitHub, a browser without a pomoi sign-in) get
  pomoi's 401 sign-in answer. This is accepted.

## Token

- Stored in the OS credential store (Keychain, Credential Manager, Secret
  Service) under its own key, never in the vault, the settings file or logs.
  Entered and removed in Settings → Editor; the field is never prefilled.
- Read lazily: on the first upload, the first pomoi image shown, or when
  Editor settings open. Launch never touches the credential store for it.

## Provider interface

`note_upload::Provider` (`upload`, `fetch`, `hosts`, `credential_key`). pomoi is
the only implementation; another service (S3/R2, an image host) implements
the same trait. The HTTP client is `okilum_http::client` for the provider's
single origin, so the Windows TLS/proxy policy applies with no exception.

## Follow-ups

- A per-drop override (modifier key or chooser).
- An offline queue: today a failed upload just keeps the local link.
- A "Manage uploads" list; removing a link never deletes the upload.
