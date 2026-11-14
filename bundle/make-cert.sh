#!/bin/bash
# Create the stable self-signed certificate that ReadSubtitles is signed with.
#
# Why this exists: macOS keys the Screen Recording (TCC) grant to an app's code
# signature. An ad-hoc signature is derived from the binary, so it changes on
# every build, and macOS treats each rebuild as a different app and asks for
# permission again. Signing with one certificate that never changes makes the
# grant stick across rebuilds.
#
# The certificate does not need to be trusted by anyone. codesign is happy to
# sign with an untrusted self-signed identity, and TCC only cares that the
# identity is stable. That is why there is no add-trusted-cert step here, and so
# no administrator password prompt.
#
# Run once. Safe to re-run: it does nothing if the identity already exists.
set -euo pipefail

NAME="ReadSubtitles Dev"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$NAME" >/dev/null 2>&1; then
  echo "certificate '$NAME' already exists; nothing to do"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "creating certificate '$NAME'..."
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
  -subj "/CN=$NAME" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null

# The legacy PBE and MAC algorithms are required: OpenSSL 3 defaults to a
# PKCS#12 MAC that macOS's security tool cannot verify, and the import fails
# with a misleading "wrong password?" error.
openssl pkcs12 -export -out "$WORK/cert.p12" \
  -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -name "$NAME" \
  -passout pass:readsubtitles \
  -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1

security import "$WORK/cert.p12" -k "$KEYCHAIN" -P readsubtitles -T /usr/bin/codesign -A

echo "done. builds will now be signed with '$NAME' automatically."
