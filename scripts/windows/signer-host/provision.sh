#!/usr/bin/env bash
# Provision the Okilum Windows signer host (#1104): CT 142 `okilum-win-signer` on sindri.
#
#   provision.sh EXPECTED_MACHINE_ID SIMPLYSIGN_BIN
#
# Idempotent; run as root inside the container. Installs the signing tools, SimplySign
# Desktop under a local-only VNC display, and forgejo-runner (registered separately by
# register-runner.sh). Holds no secret: the owner logs in to SimplySign with a one-time
# code over VNC, and the session lives only in that desktop.
set -Eeuo pipefail
expected="${1:?expected machine-id required}"
installer="${2:?SimplySign Desktop installer (.bin) required}"
# Identity before the first write: this host, not whoever answers on the address.
[ "$(cat /etc/machine-id)" = "$expected" ] || { echo "provision.sh: wrong host $(hostname)" >&2; exit 1; }
here="$(cd "$(dirname "$0")" && pwd)"

SIMPLYSIGN_SHA256=274f3e0feb5ecd40c6acc75f383917ff80d91405d5a4b5c6c4c4a9e7203ed251  # 2.9.15-9.4.5.0 ubuntu
RUNNER_VERSION=13.2.0
RUNNER_SHA256=fadaec897f5e6641c363f87ecaf75f866f99c81b7097ec6b433e4c48123fcad1
USER_NAME=okilum-signer
# SimplySign reads and writes /home/<user>/SimplySignDesktop.xml whatever $HOME says;
# elsewhere it starts with empty settings and crashes.
HOME_DIR=/home/okilum-signer

echo "$SIMPLYSIGN_SHA256  $installer" | sha256sum -c - >/dev/null

# 1. Packages: signing tools, a minimal X session for the Qt desktop app, and node for the
#    JavaScript actions (checkout, upload-artifact) that host-mode jobs run.
bash "$here/../install-signer-tools.sh" >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    tigervnc-standalone-server openbox stalonetray dbus-x11 xauth x11-utils \
    fonts-dejavu-core libgl1 libegl1 libfontconfig1 libxkbcommon-x11-0 libxcb-icccm4 \
    libxcb-image0 libxcb-keysyms1 libxcb-render-util0 libxcb-xinerama0 libxcb-xkb1 \
    libxcb-shape0 libxcb-randr0 libxcb-cursor0 libdbus-1-3 libpulse0 libpulse-mainloop-glib0 libxslt1.1 libpcsclite1 \
    novnc python3-websockify git jq python3 xz-utils nodejs >/dev/null

# 2. One unprivileged user owns the desktop, the SimplySign session and the runner, so
#    the PKCS#11 library reaches the logged-in desktop.
if id "$USER_NAME" >/dev/null 2>&1; then
    [ "$(getent passwd "$USER_NAME" | cut -d: -f6)" = "$HOME_DIR" ] || usermod --move-home --home "$HOME_DIR" "$USER_NAME"
else
    useradd --system --create-home --home-dir "$HOME_DIR" --shell /bin/bash "$USER_NAME"
fi

# 3. SimplySign Desktop, laid out like its own installer (which only prompts and copies).
if [ "$(cat /opt/SimplySignDesktop/.okilum-sha256 2>/dev/null)" != "$SIMPLYSIGN_SHA256" ]; then
    work=$(mktemp -d)
    sh "$installer" --noexec --target "$work" >/dev/null
    rm -rf /opt/SimplySignDesktop
    cp -a "$work"/SSD-*-dist /opt/SimplySignDesktop
    chmod 755 /opt/SimplySignDesktop
    echo "$SIMPLYSIGN_SHA256" > /opt/SimplySignDesktop/.okilum-sha256
    rm -rf "$work"
fi
library=$(ls /opt/SimplySignDesktop/SimplySignPKCS_64-MS-*.so | head -1)
install -d -m 755 /etc/okilum-signer
cat > /etc/okilum-signer/simplysign-pkcs11.cfg <<EOF
name = SimplySign
library = $library
slot = -1
EOF
# SimplySign keeps its settings in the user's home; seed the vendor default once.
[ -f "$HOME_DIR/SimplySignDesktop.xml" ] || install -o "$USER_NAME" -m 600 /opt/SimplySignDesktop/SimplySignDesktop.xml "$HOME_DIR/"

# 4. Desktop on display :1, reachable only from inside the container (SSH tunnel).
install -m 644 "$here/okilum-signer-vnc.service" /etc/systemd/system/
install -m 644 "$here/okilum-signer-desktop.service" /etc/systemd/system/
install -m 644 "$here/okilum-signer-novnc.service" /etc/systemd/system/
install -m 755 "$here/desktop-session.sh" /usr/local/lib/okilum-signer-desktop-session

# 5. forgejo-runner, pinned. Registration is a separate, repo-scoped step.
if [ "$(forgejo-runner --version 2>/dev/null | awk '{print $3}')" != "v$RUNNER_VERSION" ]; then
    bin=$(mktemp)
    curl -fsSL "https://code.forgejo.org/forgejo/runner/releases/download/v$RUNNER_VERSION/forgejo-runner-$RUNNER_VERSION-linux-amd64" -o "$bin"
    echo "$RUNNER_SHA256  $bin" | sha256sum -c - >/dev/null
    install -m 755 "$bin" /usr/local/bin/forgejo-runner
    rm -f "$bin"
fi
install -m 644 "$here/okilum-signer-runner.service" /etc/systemd/system/
install -d -o "$USER_NAME" -m 700 "$HOME_DIR/runner"
# Signing environment for jobs on this host; no secret in it.
cat > /etc/okilum-signer/sign.env <<EOF
OKILUM_WINDOWS_SIGN_PKCS11_CFG=/etc/okilum-signer/simplysign-pkcs11.cfg
EOF

systemctl daemon-reload
systemctl enable --now okilum-signer-vnc.service okilum-signer-desktop.service okilum-signer-novnc.service >/dev/null
echo "provisioned $(hostname) ($(cat /etc/machine-id)): $(jsign --version 2>&1 | head -1); $(osslsigncode --version 2>&1 | head -1); runner $(forgejo-runner --version | awk '{print $3}'); SimplySign $(basename "$library")"
