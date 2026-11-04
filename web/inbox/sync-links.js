// Old approval links must never mount the retained service pairing UI.
export function retiredSyncLinkMessage(hash) {
  return new URLSearchParams(hash.replace(/^#/, '')).has('sync')
    ? 'This computer connection link is no longer supported. Open Settings → Sync in Okilum on your computers to connect them. Existing file sync connections have not been changed.'
    : null;
}
