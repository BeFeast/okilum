#!/usr/bin/env bash
# Compile the actual native transport modules without GPUI or native C dependencies.
# Run under okilum-build on CT141; this is a type check, not native acceptance.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
probe="${OKILUM_SIDECAR_CHECK_DIR:-$HOME/.cache/okilum-qa/588/native-probe}"
mkdir -p "$probe/src"
python3 - "$root" "$probe" <<'PY'
import json,sys
from pathlib import Path
root, probe = map(Path, sys.argv[1:])
source = root/'crates/okilum-sync-controller/src/sidecar/mod.rs'
(probe/'src/lib.rs').write_text('#[path = '+json.dumps(str(source),ensure_ascii=False)+']\npub mod sidecar;\n')
# Keep direct versions aligned with the shipping crate, using its lockfile below.
(probe/'Cargo.toml').write_text('''[package]
name = "okilum-sidecar-native-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
anyhow = "1"
serde_json = "1"
tempfile = "3"
rustix = { version = "=1.1.4", features = ["process", "fs"] }
serde = { version = "1", features = ["derive"] }
uuid = { version = "1", features = ["v4", "serde"] }
xmltree = "0.11"
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6.4"
objc2-foundation = { version = "0.3.2", default-features = false, features = ["std", "NSString", "NSError", "NSProcessInfo"] }
objc2-service-management = { version = "0.3.2", default-features = false, features = ["std", "objc2", "objc2-foundation", "SMAppService"] }
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.61.3", features = ["Win32_Foundation", "Win32_System_Com", "Win32_System_Ole", "Win32_System_Variant", "Win32_System_TaskScheduler", "Win32_System_JobObjects", "Win32_System_Threading", "Win32_System_Pipes", "Win32_System_IO", "Win32_Security", "Win32_Security_Authorization", "Win32_Storage_FileSystem"] }
''')
(probe/'Cargo.lock').write_bytes((root/'Cargo.lock').read_bytes())
PY
for target in aarch64-apple-darwin x86_64-pc-windows-msvc; do
    cargo +1.99.0 clippy --manifest-path "$probe/Cargo.toml" --target "$target" --tests -- -D warnings
done
