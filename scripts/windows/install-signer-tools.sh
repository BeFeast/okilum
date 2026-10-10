#!/usr/bin/env bash
# Signing and verification tools for scripts/windows/sign.sh and verify-signatures.py
# (#1104). Debian 13 / Ubuntu 24.04; run as root or through sudo. Idempotent.
set -Eeuo pipefail
JSIGN_VERSION=7.5
JSIGN_SHA256=7b4a01ba81e9ee866f09a5e45d40c928707eb5286f4e20d4042c67a141ae5e62
sudo=""
[ "$(id -u)" = 0 ] || sudo=sudo
$sudo apt-get update -qq
$sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    openjdk-21-jre-headless osslsigncode opensc openssl curl ca-certificates unzip
if [ "$(dpkg-query -W -f='${Version}' jsign 2>/dev/null)" != "$JSIGN_VERSION" ]; then
    deb=$(mktemp --suffix=.deb)
    curl -fsSL "https://github.com/ebourg/jsign/releases/download/$JSIGN_VERSION/jsign_${JSIGN_VERSION}_all.deb" -o "$deb"
    echo "$JSIGN_SHA256  $deb" | sha256sum -c - >/dev/null
    $sudo dpkg -i "$deb" >/dev/null
    rm -f "$deb"
fi
jsign --version 2>&1 | head -1
osslsigncode --version 2>&1 | head -1
