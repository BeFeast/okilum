# Vault Trash on volumes without system Trash (#1924)

Oleg approved a vault-local `.trash/` fallback when macOS reports that the volume
has no system Trash. Permission, source-disappeared and generic I/O errors do not
authorize it. The initial adapter recognizes Cocoa's structured feature-unsupported
error (NSCocoaErrorDomain / NSFeatureUnsupportedError); native acceptance must
confirm that classification on the target network volume. Unknown errors fail
closed with domain/code diagnostics, never localized-text matching.

Every deletion creates `.trash/<entry UUID>/<original relative path>`. The UUID
keeps repeated deletions separate, preserving the original relative hierarchy
without overwriting another recoverable item. There is no permanent deletion or
automatic retention policy. Existing `.trash/` symlinks are refused. Operations
walk directories through no-follow descriptors; Undo checks root and source
identity, refuses an occupied destination and never adopts a different vault.

Session Undo restores the object at its original relative path. Deleting it again
creates a fresh entry (Redo behavior); existing recoverable entries remain intact.
The notification explicitly names the vault `.trash` fallback. Existing service
path filtering excludes `.trash` from tree inventory and search, with a visible
note/search positive control in the regression test. This is independent of the
read-only reader protocol and never changes the index into deletion authority.

The shared exclusive publication helper retains its documented network-directory
empty-target race from #1915; file publication remains link-based no-clobber.
Native acceptance runs only in an owned child of
`/Volumes/Maestro/okilum-nfs-qa`, including actual fallback, byte/identity-preserving
Undo, repeat deletion, no staging tails and cleanup. It must precede Sprint
transfer and merge. Live vaults, host permissions and exports are untouched.
