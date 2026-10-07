#!/usr/bin/env bash
# Source from the build user's shell. Cache failures must not prevent compilation.
# Missing credentials (including fork PRs) use the normal compiler.
# Never inherit a stale wrapper from a previous runner environment.
unset RUSTC_WRAPPER SCCACHE_SERVER_UDS SCCACHE_IDLE_TIMEOUT SCCACHE_IGNORE_SERVER_IO_ERROR
_enable_release_cache() {
  local cache_target cache_hash cache_dir cache_stage
  [[ -n ${AWS_ACCESS_KEY_ID:-} && -n ${SCCACHE_ENDPOINT:-} ]] || return 1
  # S3 may return 403 for an unsigned request; any HTTP response proves network
  # reachability. The compiler probe below verifies authenticated cache access.
  curl --silent --show-error --connect-timeout 3 --max-time 3 \
    --output /dev/null "$SCCACHE_ENDPOINT" || return 1
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
      cache_target=x86_64-unknown-linux-musl
      cache_hash=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89 ;;
    Darwin-arm64)
      cache_target=aarch64-apple-darwin
      cache_hash=308184519b646f5125289e8515b36f6ca65a13a041923994aebe702348674e8e ;;
    *) return 1 ;;
  esac
  cache_dir="${TESSERA_SCCACHE_HOME:-$HOME/.cache/tessera-sccache}/0.18.0-$cache_target"
  if [[ ! -x $cache_dir/sccache ]]; then
    mkdir -p "$cache_dir" || return 1
    cache_stage=$(mktemp -d "$cache_dir/install.XXXXXX") || return 1
    if ! _install_release_cache "$cache_stage" "$cache_target" "$cache_hash"; then
      rm -rf "$cache_stage"
      return 1
    fi
    mv "$cache_stage/sccache-v0.18.0-$cache_target/sccache" "$cache_dir/sccache" || return 1
    rm -rf "$cache_stage"
  fi
  export PATH="$cache_dir:$PATH"
  export SCCACHE_SERVER_UDS="$cache_dir/server.sock"
  export SCCACHE_IDLE_TIMEOUT=300
  export SCCACHE_IGNORE_SERVER_IO_ERROR=1
  if [[ $cache_target == aarch64-apple-darwin ]]; then
    # Job-local only. Restart the selected daemon so it inherits this shell's
    # descriptor limit, rather than reusing the runner's low-limit daemon.
    if ! { ulimit -n 8192 2>/dev/null || ulimit -n "$(ulimit -Hn)" 2>/dev/null; }; then
      echo '::warning::Cannot raise macOS descriptor limit; building without sccache'
      return 1
    fi
    local cache_nofile
    cache_nofile=$(ulimit -Sn)
    if [[ $cache_nofile != unlimited ]] && (( cache_nofile < 8192 )); then
      echo '::warning::macOS descriptor limit remains below 8192; building without sccache'
      return 1
    fi
    echo "macOS compiler cache descriptor limit: $cache_nofile"
    python3 - "$cache_dir/sccache" <<'PYRESTART' || return 1
import subprocess, sys
try:
    # Stop failure is harmless when no server was running.
    subprocess.run([sys.argv[1], '--stop-server'], timeout=3,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    result = subprocess.run([sys.argv[1], '--start-server'], timeout=3,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sys.exit(result.returncode)
except (OSError, subprocess.TimeoutExpired):
    sys.exit(1)
PYRESTART
  fi
  # A stats response is not proof that rustc can start. Bound the real wrapper
  # probe too, including daemon startup and authenticated storage initialization.
  python3 - "$cache_dir/sccache" "$(command -v rustc)" <<'PYPROBE' || return 1
import subprocess, sys
try:
    result = subprocess.run([sys.argv[1], sys.argv[2], '-vV'],
                            timeout=3, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    sys.exit(result.returncode)
except (OSError, subprocess.TimeoutExpired):
    sys.exit(1)
PYPROBE
  export RUSTC_WRAPPER="$cache_dir/sccache"
}

_install_release_cache() {
  local cache_stage="$1" cache_target="$2" cache_hash="$3"
  curl --fail --silent --show-error --location --max-time 60 \
    "https://github.com/mozilla/sccache/releases/download/v0.18.0/sccache-v0.18.0-$cache_target.tar.gz" \
    -o "$cache_stage/archive.tgz" || return 1
  python3 - "$cache_stage/archive.tgz" "$cache_hash" <<'PY' || return 1
import hashlib, sys
from pathlib import Path
if hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() != sys.argv[2]:
    raise SystemExit('sccache checksum mismatch')
PY
  tar -xzf "$cache_stage/archive.tgz" -C "$cache_stage" || return 1
}

if ! _enable_release_cache; then
  unset RUSTC_WRAPPER SCCACHE_SERVER_UDS SCCACHE_IDLE_TIMEOUT SCCACHE_IGNORE_SERVER_IO_ERROR
  echo '::warning::Shared compiler cache unavailable; building without sccache'
fi
unset -f _enable_release_cache _install_release_cache

release_cache_stats() {
  if [[ -n ${RUSTC_WRAPPER:-} && -n ${SCCACHE_SERVER_UDS:-} ]]; then sccache --show-stats || true; fi
}
