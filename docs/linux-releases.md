# Linux releases (Arch / Omarchy / CachyOS)

Okilum is distributed as the `okilum` x86_64 Arch package. Updates arrive with
your normal system upgrade (`sudo pacman -Syu`, or `omarchy update` on Omarchy);
the Linux app has no **Check for Updates** menu. A desktop entry
and icon are included. The Markdown MIME association is available through
**Open With**, but installation does not replace your default application.

## Install

This installs the stable channel, the releases offered on okilum.app. For a build
of every merge to `main`, see [Beta channel](#beta-channel).

Download the public signing key and verify its full fingerprint against the
value below **before** trusting it:

```sh
curl -fsSLo /tmp/okilum-signing-key.asc \
  https://updates.befeast.com/okilum/arch/okilum-signing-key.asc
gpg --show-keys --with-fingerprint /tmp/okilum-signing-key.asc
```

Expected fingerprint:

```text
7FFE 9F27 ECC8 E253 E45B AEA0 8AAC BFB2 C9E4 7882
```

Then import and locally trust that specific key:

```sh
sudo pacman-key --add /tmp/okilum-signing-key.asc
sudo pacman-key --lsign-key 7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882
```

Append to `/etc/pacman.conf`:

```ini
[okilum-stable]
SigLevel = Required DatabaseRequired
Server = https://updates.befeast.com/okilum/arch/stable/$arch
```

Then install. The command depends on the distribution.

**Arch Linux, CachyOS:** install with a full system upgrade (Arch partial upgrades
are unsupported):

```sh
sudo pacman -Syu okilum
pacman -Q okilum
okilum
```

Subsequent releases arrive through `sudo pacman -Syu`.

**Omarchy:** Omarchy blocks direct `pacman -Syu` with its update guard and asks
for `omarchy update`, which also takes a snapshot and runs its migrations. Upgrade
the system the Omarchy way first (this also reads the new repository), then
install the package on its own:

```sh
omarchy update
sudo pacman -S okilum
pacman -Q okilum
okilum
```

`pacman -S` without `-u` installs only Okilum, and the guard allows it. Subsequent
releases arrive through `omarchy update`. Do not bypass the guard
(`OMARCHY_ALLOW_DIRECT_PACMAN=1`) to install Okilum.

A compatible Vulkan driver is required by GPUI; Omarchy normally already has one.
Open a vault using the app's folder picker, or `okilum /path/to/vault`.

## Beta channel

Beta carries every merge to `main` and is for testing. To follow it, use this
**instead of** the stable stanza:

```ini
[okilum-beta]
SigLevel = Required DatabaseRequired
Server = https://updates.befeast.com/okilum/arch/beta/$arch
```

Do not enable both channels. Switching from a newer beta back to an older
stable does not automatically downgrade; wait for stable to catch up. Each
channel refuses backwards publication.

## Release and promotion

`.forgejo/workflows/linux-release.yml` builds each merge to `main` with shared
sccache and cached dependency downloads, in a pinned
Arch `base-devel` container, with Rust 1.96.1 and the locked dependency graph.
The package version is `0.1.<5000 + workflow run number>-1`. PRs touching packaging
build/install the package and test signing with a disposable key; they cannot
publish. Main builds publish beta. Workflow concurrency serializes publication.

For independent Linux promotion after manager GO, use the
[Linux-only releases path](releases.md#linux-only-stable-1432) with `linux_only=true`,
the accepted Linux `build` and exact full `source` SHA. It uses the existing
protected publication lock, verifies the archived package/catalog identity,
checksums and GPG signatures, and publishes the identical package bytes to stable.
It never dispatches or waits for macOS/Windows builds. Receipts support exact-input
retry and explicit restoration of the previous Linux feed. There is no rebuild
and no automatic stable promotion. The ordinary all-platform path remains available
with its accepted macOS build number.

Packages and repository databases are signed. Archives are retained under
`arch/builds/<build>/`; `manifest.json` records the source commit and checksum.
Each channel keeps older payloads so clients with an older DB can finish a download.
The live signed DB contains the latest package. DB and signature use `no-cache`.
R2 cannot replace the pair atomically: a refresh crossing publication can fail
signature verification; retry `pacman -Syyu` after publication, never disable checks.

Secrets: Infisical `services/prod/okilum`, mirrored to Forgejo Actions:
`ARCH_GPG_PRIVATE_KEY` (dedicated signing-only Ed25519 key), existing
`R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY`; `ARCH_GPG_FINGERPRINT` is an Actions
variable. The public key is also committed at `scripts/arch/okilum-signing-key.asc`.
The key expires in three years; renew/export it before expiry and update both
secret stores and the published public key. Key rotation requires communicating
and verifying the new fingerprint before changing the configured signer.

## Unsigned AppImage CI artifact (#1470)

The required hosted Linux lane builds the portable QA AppImage on Ubuntu 22.04
with GNU bfd. The retained artifact from PR #1471 at `ee7702bb` had a concrete
ELF corruption after linuxdeploy rewrote the mold 1.0.3 executable: its 13 program
headers ended at file offset `0x318`, overlapping `.rela.dyn` at `0x308`.
The last `PT_NOTE` overwrote the first relocation's offset and type, changing
`R_X86_64_RELATIVE` to `R_X86_64_PLT32` while `DT_RELACOUNT` still declared
176680 relative relocations. `ldd` passed, but actual startup failed in ld.so.

AppImage builds therefore use bfd independently of other Linux lanes and check
the relocation prefix and program-header overlap before and after deployment.
The unchanged packaged artifact must still pass actual help/window/exit smoke
on clean Ubuntu and Fedora without an external library-path override. Structural
checks do not replace that native gate. Build diagnostics and failed smoke logs
are retained as CI evidence. This path remains unsigned QA, without publication
or self-update.

## Owner acceptance

On Omarchy, with the beta stanza from «Beta channel», install beta N as above (`omarchy update`, then `sudo pacman -S okilum`)
and confirm `pacman -Q okilum`, application launch, and opening your vault. After
the next merge publishes N+1, run `omarchy update`, confirm the higher version,
relaunch and reopen the vault. CI package installation
and signature tests do not replace this desktop acceptance step.

## AppImage beta publication (#1470, slice 2)

This route reuses the unsigned portable AppImage without recompiling or changing
its bytes. After this integration lands, the public mirror's `main` push runs
`.github/workflows/forgejo-linux.yml`, the same producer used by required hosted
PR checks. There is one AppImage compilation per source. The existing public
`ci.yml` and Arch package build remain independent. A merge commit is a new
source: a successful PR-head artifact cannot be relabelled as that merge.

The Linux release bridge resolves the selected full source SHA against the original
main AppImage producer. First-attempt identity, executed successful Ubuntu/Fedora
smoke, archive digest and payload provenance authorize AppImage reuse independently
of the GitHub Linux aggregate. Missing producer, running producer, unaccepted smoke,
and absent/expired payload are typed `APPIMAGE_REUSE_SKIP` outcomes with source and
reason: reuse succeeds with `accepted=false` and produces no handoff. Malformed,
ambiguous or mismatched evidence and transport/authentication errors remain failures.
Producer, AppImage job and smoke status/conclusion must be recognized and coherent;
malformed evidence fails reuse. A cancelled original GitHub producer is explicitly
rejected even when its AppImage job and smoke succeeded.
There is no fallback build, relabelling or duplicate producer dispatch.

AppImage publication is independent of Arch acceptance: dispatch requires successful
reuse with `accepted=true` and consumes only `appimage-publication`. An Arch failure
cannot block accepted AppImage bytes. Arch dispatch still requires its successful
`release` job, and its protected publisher retains the successful aggregate gate;
it consumes only `arch-publication`. Reuse runs on every main tick even when the
Arch descriptor already exists, so later accepted AppImage evidence can publish
on the next tick without compiling another source. Cancellation still blocks publication.
Only the existing protected `publish` runner imports the Linux key and R2 credentials,
under the unchanged cross-platform publication/promotion lock.

AppImage build **N** is its original embedded `5000 + GitHub run_number`;
Forgejo's `5000 + run number` is separate `publication_build` metadata. The
publisher verifies source, version, original checksum and producer identity.
It does not patch an executable or rewrite the original `signed:false` build
provenance. The existing Linux key fingerprint is pinned:
`7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882`.

Immutable assets are:

```text
okilum/linux/builds/N/Okilum-x86_64.AppImage
okilum/linux/builds/N/Okilum-x86_64.AppImage.sig
okilum/linux/builds/N/provenance.json
okilum/linux/builds/N/release.json
okilum/linux/builds/N/release.json.sig
```

`release.json` binds source/build, artifact checksum/size, original provenance
checksum, hosted run/attempt/artifact/digest and signer. Retries verify and reuse
existing signatures; conflicting immutable bytes or identities stop before any
writes. Partial immutable uploads can resume. Every immutable object is read
back before moving `okilum/linux/beta/Okilum-x86_64.AppImage`, its `.sig`, then
`latest.json.sig` and finally `latest.json`. All moving objects use `no-cache`.
The signed latest descriptor references immutable assets. Clients crossing a
moving detached-signature update must fail closed and retry. An older AppImage
build is archived only and cannot move beta backwards. Explicit historical-source
publication also remains archive-only.

This descriptor is separate from `okilum/releases/<source>/linux.json`, which
retains its strict two-asset Arch contract. Stable, unified promotion and the
Arch update/install path are unchanged. AppImage self-update is slice 3.

For an explicitly selected beta installation, download the AppImage and detached
signature from `https://updates.befeast.com/okilum/linux/beta/`, import the public
key shown above, verify its full fingerprint, then verify before executing:

```sh
gpg --verify Okilum-x86_64.AppImage.sig Okilum-x86_64.AppImage
chmod +x Okilum-x86_64.AppImage
./Okilum-x86_64.AppImage
```

For the normal launch, install `fuse3` (Ubuntu: `sudo apt install fuse3`;
Fedora: `sudo dnf install fuse3`), which provides `fusermount3`, and ensure
`/dev/fuse` is available. The pinned runtime is statically linked: neither
`libfuse2` nor Ubuntu's `libfuse2t64` is required. Its
[source](https://github.com/AppImage/type2-runtime/blob/fedfad51acc2860b2916a1eef32ba4a500ff31ef/src/runtime/runtime.c#L417)
searches PATH for a root-owned setuid `fusermount` helper (including
`fusermount3`) that successfully reports its version.

If FUSE is unavailable or mounting fails, run the same verified download
without FUSE; no repository checkout or helper script is needed:

```sh
./Okilum-x86_64.AppImage --appimage-extract-and-run
```

This temporarily extracts the payload, runs it, and cleans up after exit;
the temporary directory needs enough space and must permit execution.
Hosted Ubuntu/Fedora smoke exercises this exact option for help, visible-window
launch and Ctrl+Q exit, with the environment-variable fallback unset. The build
also checks that the checksum-pinned runtime has no ELF interpreter or dynamic
library requirements, with a dynamic system executable as a positive control.
Direct FUSE mounting is not verified by these container checks: they have no
`/dev/fuse`.
There is no desktop-integration install or in-app update in this slice.

Premerge evidence comes from hosted tests: trust-boundary rejection, immutable
conflict/interrupted-upload/retry/rollback prevention, plus signing the actual
built AppImage with a disposable CI key, downloading/verifying/installing those
same bytes, help launch and tamper rejection. Ubuntu 24.04/Fedora 44 visible
window and Ctrl+Q tests still consume the unchanged producer bytes. Disposable
CI signing is not evidence of live-key beta publication. Live publication must
wait for parent review of this concrete path; no branch can invoke protected
signing. After merge, the accepted main producer and trusted main release run
are still needed before claiming a public signed beta exists.

The hosted signing composition test downloads the actual uploaded four-member
GitHub archive and checks its API artifact ID/digest and actual source/run/first
attempt before production unpack and original-provenance validation. Since the
workflow is still running, completion statuses and job gates in its temporary
acceptance receipt are explicit test fixtures; they never authorize publication
and are not retained as production acceptance evidence. Only this job receives
read-only Actions access using its ephemeral GitHub token. The disposable key
substitution remains inside the harness; production signer pinning is unchanged.
The installation verifier imports only the exported public key into a separate
GNUPGHOME and verifies the downloaded descriptor and referenced immutable payload.
Missing/wrong signer, changed archive/receipt/source, payload/descriptor tamper,
immutable readback corruption, interrupted immutable and moving-pair writes,
signature reuse and conflicts are exercised there with real GPG signatures.
No live-key premerge run is required; live publication remains unverified.

### Independent AppImage beta handoff (#1470)

Dispatch `linux-release.yml` on `main` with `appimage_only=true` and a full
`source` SHA from main history. This selects the source without consulting an
Arch descriptor and skips the Arch compiler job. The bridge reuses the original
GitHub AppImage archive and receipt; it never compiles, relabels or changes its
embedded build version. The independently dispatched `appimage-publication` job
depends only on successful AppImage reuse, so an Arch failure cannot block it.

The protected `release-publish.yml` routes `platform=appimage` directly to
`scripts/releases/appimage_handoff.py publish --run <linux-release-run-id>`.
That helper checks trusted main `linux-release.yml` origin, exactly one unexpired
`appimage-publication` artifact, the original GitHub repository/workflow/main
source/first attempt, executed Ubuntu/Fedora smoke, archive digest and original
payload provenance, and source ancestry on main before importing the existing
Linux signing key. Neither the Forgejo aggregate nor the GitHub Linux aggregate,
Arch artifacts or Arch feeds authorize this lane. Publication retains the shared
non-cancellable lock, pinned signer, immutable storage and monotonic beta guard.
Explicit AppImage sources can advance beta; the legacy archive-only Arch path
and all Stable behavior remain unchanged.

### AppImage-only Linux transition (approved 2026-10-23, #1564)

The approved destination is AppImage-only Linux delivery, with Linux Stable using
the signed AppImage. This handoff change does not activate that transition:
current pacman beta/Stable delivery remains available until #1564. The repository
will be frozen, not deleted, after a final pacman release communicates the switch.

Delivery order is #1488 (independent AppImage publication), #1470 slice 3
(self-update, blocking the switch), then #1564 (first-launch desktop integration,
muninn launch/vault/update acceptance, final pacman switch message, AppImage Stable
promotion and one Linux download page), followed by #1559 and #1557.
Self-update must pass N-to-N+1 acceptance on Arch and Ubuntu. The first-launch
integration must create a user-local desktop entry and icon, support removal, and
require no AppImageLauncher. These remain pending product work, not capabilities
implemented by this publication PR.

Signed AppImage build 6606 from source `0c58` has already been delivered to Dima;
no additional publication is needed for this revision. The parent owns subsequent
publication and Stable GO after the required acceptance. This change performs no
live publish and grants no new Stable GO.
