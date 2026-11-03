# Linux Manager image delivery acceptance (#2423)

## Scope and build

On 2026-11-03, the ordinary Linux application completed chooser, screenshot
paste, copied-file paste and external file drop → thumbnail → Send → an
isolated T3 server → fixture provider. No real model or working T3 session was
used. The native run used muninn's private headless sway/DBus session under a
whole-session GUI lock; screenshots contain only the owned application window
or the owned chooser dialog.

Tested product source: `f7f4585293f819443675831561c6bb89f6e665d5`.
The binary was built on baldr with the default product features, without
`ui-harness`: `cargo build --locked --offline -p okilum-shell --bin okilum`.
This is the shipping application path in the default dev build profile, not a
published AppImage. Source hashes were verified for all 2348 committed files
before building; vendor/comrak setup and verification passed. GPUI core and the
vendor patch set are unchanged.

Binary SHA256: `43fdc891039d7c1b5a048511ab3b518586e6e378e398fb68d1b87a95731a21bf`.

The unmodified T3 server was `0.1.7-nightly.20261102.4066`, on loopback with its
own HOME/profile, vault and provider settings. The native app was entered through
Reader → Projects → Manager. `scripts/qa/manager-image-provider.py` implements
only the isolated Codex app-server stdio protocol; it decodes the actual `image`
input arriving from T3 and records bytes and hashes. The loopback proxy in
`scripts/qa/manager-image-fault-proxy.py` independently hashes HTTP upload bodies,
counts dispatch command IDs, and can reject one upload or disconnect the stream.
No native QA harness flags, installed Okilum, personal credentials or real model
provider are part of this run.

## Native paths and exact bytes

Each path used an independently tagged valid PNG. The image pixels look alike,
but the encoded bytes differ, so accidentally reusing a previous clipboard,
file or upload cannot satisfy the checks. All six original files were compared
byte-for-byte with provider-captured `.bin` files. Independent T3 history asset
downloads also matched these hashes and byte counts.

| Path | Bytes | Original = provider = server download SHA256 |
| --- | ---: | --- |
| chooser | 950 | `668d60b62fd6f69c261ec55e7f77ed5a798ccbaeb7d7cbe158c0dc2af15a180e` |
| screenshot | 953 | `5897f5896dcadbeb9a9225f74b739f2abf4a1bb9bc2d27694a7b09dd67c96cc7` |
| copied file | 954 | `5d3811b815c9a1a08a0e1a4de4a50a499b6e3f0d1a82eedc4a5085c69b583c08` |
| gnome file | 953 | `7b3afc19c693077ad991dc9d51144adcc0e7d4bf626600eaca8ec0624fecbaa2` |
| drop | 947 | `3767121eb78f497f702742d7a5e282570f9c52781a04d7115dcb3ffa6475ba25` |
| retry | 948 | `0ace0407eca6f7f853642a881ef7b486f55601131027f93fa5b561fdc8558619` |

- Chooser: actual Attach button and desktop portal GTK Open File dialog, with
  a filename containing a unique fixture tag; confirmed tray thumbnail, then Send.
- Screenshot: `image/png` Wayland clipboard data and actual Ctrl+V. Restarted
  Okilum with the uploaded draft before Send: its pending asset ID was unchanged,
  one upload and zero dispatches/provider turns remained, and the thumbnail
  restored. Sending that restored draft delivered its original bytes.
- Copied file: actual Ctrl+V with `text/uri-list`, including a space in the path.
- GNOME copied file: actual Ctrl+V offering only
  `x-special/gnome-copied-files` (`copy` plus file URI). This exercises the Linux
  flavor missing before the fix.
- Drop: an external GTK drag source delivered `text/uri-list` onto the composer;
  its freshly cleared `drag-data-get` marker fired as a positive control. Sent
  the image without any accompanying text.
- Retry: the proxy rejected one binary upload with HTTP 503. The row visibly
  retained the thumbnail/text and showed Upload failed, the status and Retry.
  Changed the clipboard to another fixture before clicking Retry; the original
  captured retry bytes reached the provider.

Native rejection controls attached a 10 MiB + 1 image and an unsupported SVG.
Both displayed a specific error, preserved text, blocked Send, and issued no
upload/dispatch. Removing the rows recovered the composer.

## Restart, reconnect and duplicate controls

Final counters: **6 user messages, 6 completed provider turns, 6 unique dispatch
commands, 7 upload POSTs (6 successful plus the deliberate 503)**. Only Retry's
hash occurred twice. All six immutable journal records were acknowledged and
matched the server message IDs.

The final server history (message IDs, promoted attachment IDs, sizes, runs and
pending count) was identical before restart, after restart and after reconnect.
A real native process stop/start restored the sent image; clicking its thumbnail
opened the full image viewer. The proxy then closed current sockets and observed
a fresh subscription with **`afterSequence: 349`**, proving that reconnect ran.
The image remained visible and no upload, dispatch or provider turn was added.
T3 promotes pending asset IDs to thread asset IDs on Send; equality is checked
across history snapshots and by actual bytes, not between pending and published IDs.

## Regression coverage and evidence

The baldr build receipt reports exit 0 and zero OOM kills. All **15 focused tests
passed**, without skips: five native `image_delivery_tests` (actual chooser,
paste/drop handlers, binary HTTP, Send and journal), seven preparation tests,
two chat clipboard fallback tests, and the GNOME flavor parser test. The Retry
regression changes the external file before retrying; reopened acknowledged
journals and connection refresh leave upload/dispatch counts unchanged.

Full Linux integration runs through `H=github scripts/agent/gh-check.sh`; the PR
carries its current status. Known unrelated full-shell baseline failures are
tracked separately in #2428.

Evidence attachments use `.txt` where Forgejo does not accept the source extension.

- [Native byte/count receipt](https://git.oklabs.uk/attachments/7b0cf297-d387-42e0-93b5-e21fdbd3f0d5)
- [Upload, dispatch and reconnect observations](https://git.oklabs.uk/attachments/8cbb8529-5fcf-4b7a-abae-be97db5c13f9)
- [Actual provider stdio input and image hashes](https://git.oklabs.uk/attachments/3380c006-6b6a-4da9-844f-d1e85cec2dcf)
- [Independent history asset downloads](https://git.oklabs.uk/attachments/c9bd1914-6249-425a-b525-4708de196a87)
- [Receipt verification script](https://git.oklabs.uk/attachments/62d08c9a-0c48-43ad-9b07-1c09f866a69a)
- [Build and focused-test log](https://git.oklabs.uk/attachments/cf7151f7-a0ef-4dd6-82c5-9a3ddbefaa10)
- [Build-slot receipt](https://git.oklabs.uk/attachments/c2c44bfb-54af-4bae-b82e-143163f7733c)
- [Actual chooser dialog](https://git.oklabs.uk/attachments/27e5c30a-5cd6-4726-9e00-84ab010a6d05)
- [Chooser thumbnail](https://git.oklabs.uk/attachments/96146ff3-c09a-4482-b8b2-6b605797bb1c)
- [Screenshot thumbnail](https://git.oklabs.uk/attachments/abedc7a1-ff9f-4077-8e77-f706faa14638)
- [Copied-file thumbnail](https://git.oklabs.uk/attachments/6bb89354-e4cd-497b-8806-6c7872084cff)
- [GNOME-only copied-file thumbnail](https://git.oklabs.uk/attachments/66997ec3-d815-458b-b970-998a5132ed50)
- [External-drop thumbnail](https://git.oklabs.uk/attachments/a842e6ee-22cf-476f-86ce-732b4e1d7490)
- [Uploaded draft after restart](https://git.oklabs.uk/attachments/acc58e07-9c90-474c-9fbe-1271bbf8757d)
- [Injected upload failure and Retry](https://git.oklabs.uk/attachments/264ac64a-7e80-4c1f-8cb0-a6eef1c2f6e8)
- [Retry recovered](https://git.oklabs.uk/attachments/3147e36a-35c9-4bcd-84e0-eb07347a9123)
- [Oversized image rejected](https://git.oklabs.uk/attachments/3c1a9e11-4c37-4379-9877-f827636367bb)
- [Unsupported image rejected](https://git.oklabs.uk/attachments/0e800215-6b26-47e5-8f29-09da5c483d78)
- [Sent image after restart](https://git.oklabs.uk/attachments/0265f164-9356-4123-8703-c5d48c7c818c)
- [Full viewer after restart](https://git.oklabs.uk/attachments/e26b64ed-448b-4ca6-bba8-89d3fcb41ac0)
- [Sent image after reconnect](https://git.oklabs.uk/attachments/d7f69dd8-c028-4253-833a-5289c32f2747)
