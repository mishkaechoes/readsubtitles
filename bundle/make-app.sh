#!/bin/bash
# Assemble ReadSubtitles.app.
#
# Bundling is not optional here. ScreenCaptureKit is TCC-gated with no
# entitlement escape hatch, the system kills an app that raises the Screen
# Recording prompt without NSScreenCaptureUsageDescription in its Info.plist,
# and on macOS 26+ an unbundled binary does not even appear in the Screen
# Recording list.
#
# Signing identity matters more than it looks. TCC keys its grant to the code
# signature, so an ad-hoc signature (which changes with every build) makes macOS
# re-prompt after each rebuild. That is merely annoying for a developer and a
# real problem for a blind user who cannot see the dialog. Set SIGN_IDENTITY to
# a stable self-signed certificate to keep the grant across rebuilds:
#
#   SIGN_IDENTITY="ReadSubtitles Dev" ./bundle/make-app.sh
#
set -euo pipefail

cd "$(dirname "$0")/.."

# Find cargo even when the shell running this has not sourced a Rust profile
# (Homebrew's rustup keeps its shims outside the default PATH).
if ! command -v cargo >/dev/null 2>&1; then
  for dir in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin; do
    [ -x "$dir/cargo" ] && PATH="$dir:$PATH" && break
  done
fi
command -v cargo >/dev/null 2>&1 || { echo "cargo not found; install Rust first" >&2; exit 1; }
APP="target/ReadSubtitles.app"
# Prefer the stable certificate when it exists, so the Screen Recording grant
# survives rebuilds. bundle/make-cert.sh creates it.
STABLE="ReadSubtitles Dev"
if [ -n "${SIGN_IDENTITY:-}" ]; then
  IDENTITY="$SIGN_IDENTITY"
elif security find-certificate -c "$STABLE" >/dev/null 2>&1; then
  IDENTITY="$STABLE"
else
  IDENTITY="-"
fi

echo "building release binary..."
cargo build --release -p readsubtitles-app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp bundle/Info.plist "$APP/Contents/Info.plist"
cp target/release/readsubtitles "$APP/Contents/MacOS/readsubtitles"

echo "signing with identity: $IDENTITY"
codesign --force --sign "$IDENTITY" --timestamp=none "$APP"
codesign --verify --verbose=1 "$APP" 2>&1 | sed 's/^/  /'

if [ "$IDENTITY" = "-" ]; then
  echo
  echo "NOTE: signed ad-hoc, so macOS will ask for Screen Recording permission"
  echo "again after every rebuild. Run ./bundle/make-cert.sh once to fix that."
fi

echo
echo "built $APP"
echo "run it with: open $APP"
