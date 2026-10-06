#!/usr/bin/env bash
# Source from the build user's shell. Never persist credentials in the checkout.
# Missing credentials (including fork PRs) use the normal compiler.
if [[ -n ${AWS_ACCESS_KEY_ID:-} && -n ${SCCACHE_ENDPOINT:-} ]]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
      cache_target=x86_64-unknown-linux-musl
      cache_hash=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89 ;;
    Darwin-arm64)
      cache_target=aarch64-apple-darwin
      cache_hash=308184519b646f5125289e8515b36f6ca65a13a041923994aebe702348674e8e ;;
    *) echo 'Unsupported sccache host' >&2; return 1 ;;
  esac
  cache_dir="$HOME/.cache/tessera-sccache/0.18.0-$cache_target"
  if [[ ! -x $cache_dir/sccache ]]; then
    mkdir -p "$cache_dir"
    curl --fail --silent --show-error --location \
      "https://github.com/mozilla/sccache/releases/download/v0.18.0/sccache-v0.18.0-$cache_target.tar.gz" \
      -o "$cache_dir/archive.tgz"
    python3 - "$cache_dir/archive.tgz" "$cache_hash" <<'PY'
import hashlib, sys
from pathlib import Path
if hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() != sys.argv[2]:
    raise SystemExit('sccache checksum mismatch')
PY
    tar -xzf "$cache_dir/archive.tgz" --strip-components=1 -C "$cache_dir"
    rm "$cache_dir/archive.tgz"
  fi
  export PATH="$cache_dir:$PATH"
  # Isolate from another runner job/user's server configuration on shared hosts.
  export SCCACHE_SERVER_UDS="$cache_dir/server.sock"
  export SCCACHE_IDLE_TIMEOUT=300
  export SCCACHE_IGNORE_SERVER_IO_ERROR=1
  if sccache --start-server || sccache --show-stats >/dev/null; then
    export RUSTC_WRAPPER="$cache_dir/sccache"
  else
    echo 'sccache unavailable; using the compiler directly' >&2
  fi
else
  echo 'No shared cache credentials; using the compiler directly'
fi

release_cache_stats() {
  if [[ -n ${SCCACHE_SERVER_UDS:-} ]]; then sccache --show-stats; fi
}
