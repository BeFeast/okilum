# Native Manager attachment transport (#2122)

The approved Manager chat capability spec uses the existing T3 native audience.
`NativeClient::upload_attachment` mints an `attachments.createUploadUrl`, then
posts the actual binary bytes to its same-origin signed URL. A confirmed HTTP
204 returns a `ChatAttachment` containing the server ID and display metadata.
Transfer URLs and local file handles are session data, outside this reference.

The deployed T3 contract at `0.1.7-nightly.20261031.3991` accepts PNG, JPEG, GIF
and WebP uploads up to 10 MiB each, with an 80 MiB total image budget per message.
Generic files have a 50 MiB per-file limit. Existing history references can use
other `image/*` MIME types; upload support and history metadata are distinct.
No additional attachment-count limit is invented by this adapter.

`download_attachment` requests a fresh `assets.createUrl`, reads the bounded
body and verifies the declared byte count. Signed HTTP transfers carry no
native bearer and do not follow redirects. Errors omit peer bodies and URLs.
An unconfirmed upload cleans up its unpublished ID on the original server when
the network lease permits. Explicit `delete_attachment` is for unsent draft
assets; callers must retain any reference handed to the command journal.

`Intent::message_with_attachments` supports attachment-only messages and stores
the complete immutable references before dispatch. Reopened journals retry the
same command, message and asset IDs. Legacy text-only records deserialize with
an empty attachment list; acknowledged records never dispatch again.

Composer upload/draft controls and history rendering integrate this transport
in the Manager chat UI. The durable draft contract retains display rows across
restart; unuploaded local bytes require explicit reselect.

Regression evidence includes binary HTTP upload/download, same-origin and
expiry checks, HTTP error/redirect/lost-ACK cleanup, truncated downloads, byte
limits, attachment-only sends and legacy/lost-ACK journal round trips. The
ignored real-server harness transfers isolated PNG/TXT assets and deletes them;
it dispatches no thread commands.
