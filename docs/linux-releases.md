# Linux releases (Arch / Omarchy / CachyOS)

Tessera is distributed as the `tessera` x86_64 Arch package. Updates use
`pacman -Syu`; the Linux app has no **Check for Updates** menu. A desktop entry
and icon are included. The Markdown MIME association is available through
**Open With**, but installation does not replace your default application.

## Install beta

Download the public signing key and verify its full fingerprint against the
value below **before** trusting it:

```sh
curl -fsSLo /tmp/tessera-signing-key.asc \
  https://updates.befeast.com/tessera/arch/tessera-signing-key.asc
gpg --show-keys --with-fingerprint /tmp/tessera-signing-key.asc
```

Expected fingerprint:

```text
7FFE 9F27 ECC8 E253 E45B AEA0 8AAC BFB2 C9E4 7882
```

Then import and locally trust that specific key:

```sh
sudo pacman-key --add /tmp/tessera-signing-key.asc
sudo pacman-key --lsign-key 7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882
```

Append to `/etc/pacman.conf`:

```ini
[tessera-beta]
SigLevel = Required DatabaseRequired
Server = https://updates.befeast.com/tessera/arch/beta/$arch
```

Install with a full system upgrade (Arch partial upgrades are unsupported):

```sh
sudo pacman -Syu tessera
pacman -Q tessera
tessera
```

Subsequent builds arrive through `sudo pacman -Syu`. A compatible Vulkan driver
is required by GPUI; Omarchy normally already has one. Open a vault using the
app's folder picker, or `tessera /path/to/vault`.

## Stable channel

After a build is promoted, use this **instead of** the beta stanza:

```ini
[tessera-stable]
SigLevel = Required DatabaseRequired
Server = https://updates.befeast.com/tessera/arch/stable/$arch
```

Stable becomes available with its first promotion. Do not enable both channels.
Switching from a newer beta to an older stable does not automatically downgrade;
wait for stable to catch up. Each channel refuses backwards publication.

## Release and promotion

`.forgejo/workflows/linux-release.yml` builds each merge to `main` in a pinned
Arch `base-devel` container, with Rust 1.96.1 and the locked dependency graph.
The package version is `0.1.<5000 + workflow run number>-1`. PRs touching packaging
build/install the package and test signing with a disposable key; they cannot
publish. Main builds publish beta. Workflow concurrency serializes publication.

To promote, use the [unified releases workflow](releases.md) with the accepted
macOS build number; the Arch build is selected by the same source commit. It
verifies the archived package and GPG signature and publishes the identical bytes
to stable. There is no rebuild and no automatic stable promotion.

Packages and repository databases are signed. Archives are retained under
`arch/builds/<build>/`; `manifest.json` records the source commit and checksum.
Each channel keeps older payloads so clients with an older DB can finish a download.
The live signed DB contains the latest package. DB and signature use `no-cache`.
R2 cannot replace the pair atomically: a refresh crossing publication can fail
signature verification; retry `pacman -Syyu` after publication, never disable checks.

Secrets: Infisical `services/prod/tessera`, mirrored to Forgejo Actions:
`ARCH_GPG_PRIVATE_KEY` (dedicated signing-only Ed25519 key), existing
`R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY`; `ARCH_GPG_FINGERPRINT` is an Actions
variable. The public key is also committed at `scripts/arch/tessera-signing-key.asc`.
The key expires in three years; renew/export it before expiry and update both
secret stores and the published public key. Key rotation requires communicating
and verifying the new fingerprint before changing the configured signer.

## Owner acceptance

On Omarchy, install beta N and confirm `pacman -Q tessera`, application launch,
and opening your vault. After the next merge publishes N+1, run `sudo pacman -Syu`,
confirm the higher version, relaunch and reopen the vault. CI package installation
and signature tests do not replace this desktop acceptance step.
