#!/bin/bash
# Pinned cross-platform packager; .NET is required on the build host only.
set -Eeuo pipefail
root="${1:?tool directory required}"
mkdir -p "$root"
curl -fsSL https://github.com/velopack/velopack/releases/download/1.2.161/vpk.1.2.161.nupkg -o "$root/vpk.nupkg"
echo "2b56ce117f803fc70c103cb423bd040e395e40370f6ff6e10818e9ff9c26a323  $root/vpk.nupkg" | sha256sum -c -
python3 - "$root" <<'PY'
import pathlib, sys, zipfile
root = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(root / 'vpk.nupkg') as archive:
    archive.extractall(root / 'vpk')
PY
# Runtime hash from Microsoft's 8.0 release metadata; no remote install script.
curl -fsSL https://builds.dotnet.microsoft.com/dotnet/Runtime/8.0.31/dotnet-runtime-8.0.31-linux-x64.tar.gz -o "$root/dotnet.tar.gz"
echo "f336bdec58d54bf50d74a1b38efa82f7290d976bd2ed98b845ebcbac42cf0d8cef504684fc088d4b05f98737b996bf3302e52bd9c23005deae7c60780b2652fb  $root/dotnet.tar.gz" | sha512sum -c -
mkdir -p "$root/dotnet"
tar -xzf "$root/dotnet.tar.gz" -C "$root/dotnet"
