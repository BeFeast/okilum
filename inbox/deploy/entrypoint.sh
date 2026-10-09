#!/bin/sh
set -eu
if [ "$#" -gt 0 ]; then exec "$@"; fi
set -- okilum-inboxd serve --data-dir /data --origin "$INBOX_ORIGIN" \
  --fixture-vault /fixture-vault \
  --vault-folder Projects --vault-folder Areas --vault-folder Resources --vault-folder Archives
if [ -n "${AI_ENDPOINT:-}" ]; then
  : "${AI_MODEL:?Set AI_MODEL when enabling AI}"
  set -- "$@" --ai-endpoint "$AI_ENDPOINT" --ai-model "$AI_MODEL" --ai-credential-file /run/credentials/cliproxy-key
fi
exec "$@"
