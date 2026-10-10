#!/usr/bin/env bash
# A throwaway self-signed code-signing identity for OKILUM_WINDOWS_SIGN_BACKEND=pkcs12-test
# (#1104). Its outputs exercise signing and verification end to end and are never published.
#
#   eval "$(make-test-signer.sh DIR)"    # exports the pkcs12-test environment
set -Eeuo pipefail
dir="${1:?output directory required}"
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
umask 077
openssl rand -hex 24 > "$dir/password"
openssl req -x509 -newkey rsa:3072 -sha256 -days 2 -nodes -keyout "$dir/key.pem" -out "$dir/cert.pem" \
    -subj "/CN=Okilum pkcs12-test (not trusted)/O=Okilum CI" \
    -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=codeSigning" 2>/dev/null
openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" -name okilum-test \
    -passout "file:$dir/password" -out "$dir/test.p12"
rm -f "$dir/key.pem"
sha256=$(openssl x509 -in "$dir/cert.pem" -outform DER | sha256sum | cut -d' ' -f1)
subject=$(openssl x509 -in "$dir/cert.pem" -noout -subject -nameopt RFC2253 | sed 's/^subject=//')
cat <<EOF
export OKILUM_WINDOWS_SIGN_BACKEND=pkcs12-test
export OKILUM_WINDOWS_SIGN_TEST_P12='$dir/test.p12'
export OKILUM_WINDOWS_SIGN_TEST_PASSWORD_FILE='$dir/password'
export OKILUM_WINDOWS_SIGN_ALIAS=okilum-test
export OKILUM_WINDOWS_SIGN_CERT_SHA256=$sha256
export OKILUM_WINDOWS_SIGN_SUBJECT='$subject'
export OKILUM_WINDOWS_SIGN_CA_FILE='$dir/cert.pem'
EOF
