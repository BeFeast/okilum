#!/usr/bin/env bash
# Authenticode-sign Windows PE files: the only signing entry point (#1104).
#
#   sign.sh FILE...    sign every file in one jsign call
#   sign.sh --probe    exit 0 only if the key is reachable and its certificate is the pinned one
#
# Velopack calls this through `--signTemplate "<path>/sign.sh {{file...}}"`. On Linux it
# escapes `$`, quotes and backticks in the template, so all configuration comes from the
# environment it inherits:
#   OKILUM_WINDOWS_SIGN_BACKEND      certum-pkcs11 | esigner | pkcs12-test
#   OKILUM_WINDOWS_SIGN_CERT_SHA256  pinned signer certificate (hex, any case, colons allowed)
#   certum-pkcs11: OKILUM_WINDOWS_SIGN_PKCS11_CFG (jsign/SunPKCS11 config with library=),
#                  OKILUM_WINDOWS_SIGN_ALIAS, optional OKILUM_WINDOWS_SIGN_PIN_FILE (0600)
#   esigner:       OKILUM_WINDOWS_SIGN_ALIAS (credential id), ESIGNER_STOREPASS
#                  ("<username>|<password>"), ESIGNER_TOTP_SECRET (base64)
#   pkcs12-test:   OKILUM_WINDOWS_SIGN_TEST_P12, OKILUM_WINDOWS_SIGN_TEST_PASSWORD_FILE,
#                  OKILUM_WINDOWS_SIGN_ALIAS
# Secrets are passed to jsign as env:/file: references, never on its command line.
set -Eeuo pipefail

die() { echo "sign.sh: $*" >&2; exit 1; }

# A pull request runs code nobody has reviewed yet: it must never reach a signing key.
[ "${GITHUB_EVENT_NAME:-}" != pull_request ] || die "refusing to sign in a pull_request run"
backend="${OKILUM_WINDOWS_SIGN_BACKEND:-}"
[ -n "$backend" ] || die "OKILUM_WINDOWS_SIGN_BACKEND is not set"
jsign="${OKILUM_JSIGN:-jsign}"
tsa="${OKILUM_WINDOWS_SIGN_TSA:-http://time.certum.pl}"

normalize() { tr -d ': \r\n' | tr '[:upper:]' '[:lower:]'; }

store=()
case "$backend" in
    certum-pkcs11)
        cfg="${OKILUM_WINDOWS_SIGN_PKCS11_CFG:?PKCS#11 config required}"
        store=(--storetype PKCS11 --keystore "$cfg" --alias "${OKILUM_WINDOWS_SIGN_ALIAS:?alias required}")
        # SimplySign did not ask for a PIN at activation; the file exists only if the pilot shows it must.
        if [ -n "${OKILUM_WINDOWS_SIGN_PIN_FILE:-}" ]; then
            store+=(--storepass "file:$OKILUM_WINDOWS_SIGN_PIN_FILE")
        fi
        ;;
    esigner)
        store=(--storetype ESIGNER --storepass env:ESIGNER_STOREPASS
               --alias "${OKILUM_WINDOWS_SIGN_ALIAS:?credential id required}" --keypass env:ESIGNER_TOTP_SECRET)
        ;;
    pkcs12-test)
        store=(--storetype PKCS12 --keystore "${OKILUM_WINDOWS_SIGN_TEST_P12:?test keystore required}"
               --storepass "file:${OKILUM_WINDOWS_SIGN_TEST_PASSWORD_FILE:?test password file required}"
               --alias "${OKILUM_WINDOWS_SIGN_ALIAS:?alias required}")
        ;;
    *) die "unknown backend $backend" ;;
esac

# The certificate the configured key presents, as a lower-case SHA-256 of its DER.
# For PKCS#11 it is read through the same SunPKCS11 keystore and alias jsign signs with:
# SimplySign shows its objects only after a login (no PIN), which pkcs11-tool does not do.
certificate_sha256() {
    local der
    der=$(mktemp)
    trap 'rm -f "$der"' RETURN
    case "$backend" in
        certum-pkcs11)
            local pass=""
            [ -z "${OKILUM_WINDOWS_SIGN_PIN_FILE:-}" ] || pass=$(cat "$OKILUM_WINDOWS_SIGN_PIN_FILE")
            PASS="$pass" keytool -exportcert -alias "$OKILUM_WINDOWS_SIGN_ALIAS" -keystore NONE \
                -storetype PKCS11 -providerClass sun.security.pkcs11.SunPKCS11 -providerArg "$cfg" \
                -storepass:env PASS -file "$der" >/dev/null 2>&1 || return 1
            ;;
        pkcs12-test)
            openssl pkcs12 -in "$OKILUM_WINDOWS_SIGN_TEST_P12" -nokeys -clcerts \
                -passin "file:$OKILUM_WINDOWS_SIGN_TEST_PASSWORD_FILE" 2>/dev/null \
                | openssl x509 -outform DER -out "$der" 2>/dev/null || return 1
            ;;
        *) return 2 ;;  # eSigner has no local token to read
    esac
    [ -s "$der" ] || return 1
    sha256sum "$der" | cut -d' ' -f1
}

if [ "${1:-}" = --probe ]; then
    expected=$(printf '%s' "${OKILUM_WINDOWS_SIGN_CERT_SHA256:?pinned certificate SHA-256 required}" | normalize)
    status=0
    actual=$(certificate_sha256) || status=$?
    if [ "$status" = 2 ]; then
        echo "sign.sh: $backend has no local token; the signature check after signing pins the certificate"
        exit 0
    fi
    [ "$status" = 0 ] || die "probe: no certificate on the $backend token (SimplySign session closed?)"
    [ "$actual" = "$expected" ] || die "probe: the token presents certificate $actual, expected $expected"
    echo "sign.sh: probe ok, certificate $actual"
    exit 0
fi

[ "$#" -gt 0 ] || die "no files to sign"
for file in "$@"; do [ -f "$file" ] || die "not a file: $file"; done

# SimplySign enumerates the token before its certificate is populated, so right after a
# login jsign reports "No certificate found" for a while: retry that every 4 s for ~60 s.
# Any other failure (usually the timestamp server) gets 3 tries, 10 s apart.
short="${OKILUM_WINDOWS_SIGN_WAIT:-4}" long="${OKILUM_WINDOWS_SIGN_RETRY:-10}"  # seconds; tests shorten them
waiting=0 failures=0
while :; do
    log=$(mktemp)
    if "$jsign" "${store[@]}" --alg SHA-256 --tsaurl "$tsa" --tsmode RFC3161 --tsretries 3 --tsretrywait 10 \
            --name Okilum --url https://okilum.app "$@" >"$log" 2>&1; then
        cat "$log"; rm -f "$log"
        exit 0
    fi
    if grep -q 'No certificate found' "$log" && [ "$waiting" -lt 15 ]; then
        waiting=$((waiting + 1)); rm -f "$log"; sleep "$short"; continue
    fi
    failures=$((failures + 1))
    if [ "$failures" -ge 3 ] || grep -q 'No certificate found' "$log"; then
        cat "$log" >&2; rm -f "$log"
        die "jsign failed for: $*"
    fi
    rm -f "$log"; sleep "$long"
done
