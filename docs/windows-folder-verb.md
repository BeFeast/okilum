# Open folders as vaults from Explorer (#1914)

Installed Windows builds add **Open in Okilum as Vault** for folders and folder
backgrounds. Windows11 exposes this classic verb under **Show more options**.
Velopack install/update hooks register it per user in HKCU; the existing uninstall
hook removes exactly the two BeFeast.Okilum.OpenVault subtrees. Shared Directory
keys, other applications' verbs, defaults and Explorer UserChoice are preserved.
Portable startup registers nothing.

The quoted command calls the ordinary `okilum --vault <folder>` entry point.
An appended `\.` path component preserves spaces, Unicode and quoted drive/UNC
roots without a trailing-backslash quoting ambiguity. The existing Reader
single-instance request and vault resolution/window policy are reused; no folder
picker or parallel vault identity model is introduced.

Core regressions check the exact owned keys/arguments, real Windows argument
parsing and registry round-trip/idempotence with a preserved neighboring verb.
The premerge installed-PR scenario is described in
[scripts/windows/qa-harness/folder-verb/README.md](../scripts/windows/qa-harness/folder-verb/README.md).
Its original-artifact, Shell Directory Verbs()/DoIt cold/warm, uninstall and unchanged
vault proofs must pass on VM150 before this installer change can merge.

This PR is the Windows slice. The existing Mac Service candidate is retained on
local branch candidate/1914-mac-integrated; its native install/invoke/uninstall
gate has not run and it is not included here. Linux .desktop action/helper
integration is also outstanding. Issue1914 remains open for those slices.
