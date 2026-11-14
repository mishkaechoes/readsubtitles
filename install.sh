#!/bin/bash
# Install ReadSubtitles.
#
# One command that does everything: checks what is missing, offers to install
# it, creates the signing certificate, builds, puts the app in /Applications,
# and opens the one System Settings pane that has to be clicked by a human.
#
# Safe to re-run. Every step checks whether it is already done, so running this
# again after a `git pull` just rebuilds.
#
# It speaks as it goes, because the person most likely to be sitting here is
# the one who cannot read the terminal. Pass --quiet to turn that off.
set -euo pipefail

cd "$(dirname "$0")"

QUIET=0
SKIP_EXTENSION=0
for arg in "$@"; do
  case "$arg" in
    --quiet) QUIET=1 ;;
    --no-extension) SKIP_EXTENSION=1 ;;
    -h|--help)
      echo "usage: ./install.sh [--quiet] [--no-extension]"
      echo
      echo "  --quiet          do not speak progress aloud"
      echo "  --no-extension   skip the optional browser extension setup"
      exit 0
      ;;
    *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

BOLD=$'\033[1m'; DIM=$'\033[2m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; OFF=$'\033[0m'
step()  { printf '\n%s==>%s %s%s%s\n' "$GREEN" "$OFF" "$BOLD" "$1" "$OFF"; }
info()  { printf '    %s\n' "$1"; }
warn()  { printf '%s    %s%s\n' "$YELLOW" "$1" "$OFF"; }
fail()  { printf '\n%serror:%s %s\n' "$RED" "$OFF" "$1" >&2; exit 1; }
# Speech is best-effort: a missing voice must never stop an install.
speak() { [ "$QUIET" = 1 ] || say "$1" >/dev/null 2>&1 || true; }

ask() {
  # Default yes, and assume yes when nothing is attached to stdin, so this
  # works unattended in a script or a CI job.
  local prompt="$1"
  [ -t 0 ] || return 0
  printf '\n    %s [Y/n] ' "$prompt"
  read -r reply
  case "$reply" in [nN]*) return 1 ;; *) return 0 ;; esac
}

printf '%s\n' "${BOLD}ReadSubtitles${OFF} — reads the subtitles on your screen out loud"
speak "Installing Read Subtitles."

# ---------------------------------------------------------------- macOS
step "Checking macOS"
MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
[ "$MAJOR" -ge 13 ] || fail "macOS 13 or later is required; this is $(sw_vers -productVersion)."
info "macOS $(sw_vers -productVersion), fine"

# ------------------------------------------------- Xcode Command Line Tools
step "Checking the Xcode Command Line Tools"
if xcode-select -p >/dev/null 2>&1 && [ -x "$(xcode-select -p)/usr/bin/clang" ] 2>/dev/null \
   || command -v clang >/dev/null 2>&1; then
  info "already installed"
else
  warn "not installed. macOS will show a download window."
  speak "macOS needs to download the developer tools. Please accept the dialog."
  xcode-select --install 2>/dev/null || true
  info "waiting for the download to finish (this can take several minutes)..."
  # The installer runs detached, so poll rather than wait on a pid.
  until command -v clang >/dev/null 2>&1; do sleep 10; done
  info "installed"
fi

# ---------------------------------------------------------------- Rust
step "Checking Rust"
# Homebrew's rustup keeps its shims off the default PATH, so look there too.
for dir in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin; do
  [ -x "$dir/cargo" ] && PATH="$dir:$PATH"
done
if command -v cargo >/dev/null 2>&1; then
  info "$(rustc --version 2>/dev/null || echo 'cargo found')"
else
  warn "Rust is not installed. It is needed to build the app."
  if ask "Install Rust now, from https://rustup.rs?"; then
    speak "Installing Rust. This takes a couple of minutes."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    PATH="$HOME/.cargo/bin:$PATH"
    info "$(rustc --version)"
  else
    fail "Rust is required. Install it from https://rustup.rs and run this again."
  fi
fi

# ort is the ONNX binding used for telling voices apart, and it sets the floor.
NEEDED=1.92
HAVE="$(rustc --version | awk '{print $2}' | cut -d- -f1)"
if [ "$(printf '%s\n%s\n' "$NEEDED" "$HAVE" | sort -V | head -1)" != "$NEEDED" ]; then
  warn "Rust $HAVE is older than the $NEEDED this needs."
  if command -v rustup >/dev/null 2>&1 && ask "Update Rust now?"; then
    rustup update stable && rustup default stable
    info "$(rustc --version)"
  else
    fail "Rust $NEEDED or later is required. Run: rustup update stable"
  fi
fi

# ------------------------------------------------------------ certificate
step "Setting up the signing certificate"
# Without a stable signature macOS re-asks for Screen Recording after every
# rebuild, which is a real obstacle for someone who cannot see the dialog.
./bundle/make-cert.sh | sed 's/^/    /'

# ------------------------------------------------------------------ build
step "Building"
speak "Building. This takes a few minutes the first time."
info "first build takes a few minutes; later ones are seconds"
./bundle/make-app.sh 2>&1 | sed 's/^/    /'

# ---------------------------------------------------------------- install
step "Installing to /Applications"
if pgrep -f "ReadSubtitles.app" >/dev/null 2>&1; then
  info "quitting the running copy"
  pkill -f "ReadSubtitles.app" || true
  sleep 1
fi
rm -rf /Applications/ReadSubtitles.app
cp -R target/ReadSubtitles.app /Applications/
info "installed at /Applications/ReadSubtitles.app"

# -------------------------------------------------------------- extension
if [ "$SKIP_EXTENSION" = 0 ]; then
  step "Browser extension (optional)"
  info "Reading captions from the browser is faster than reading the screen,"
  info "gives exact text instead of OCR, and needs no Screen Recording permission."
  info "It only works on sites the extension covers; the screen source works everywhere."
  if ask "Set up the browser extension?"; then
    ./bundle/make-extension.sh | sed 's/^/    /'
  else
    info "skipped. Run ./bundle/make-extension.sh later if you change your mind."
  fi
fi

# ------------------------------------------------------------- permission
step "Screen Recording permission"
speak "One permission left. Please tick Read Subtitles in the window that just opened."
info "macOS will not let an app ask for this on its own, so it has to be ticked by hand."
info "Opening System Settings at the right pane now."
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture" 2>/dev/null || \
  warn "could not open it; go to System Settings > Privacy & Security > Screen Recording"
info ""
info "    Turn on the switch next to ${BOLD}ReadSubtitles${OFF}."
info ""
info "This is a one-off, and the certificate above keeps it that way across updates."

# ------------------------------------------------------------------- done
step "Done"
open /Applications/ReadSubtitles.app
cat <<INFO

    ${BOLD}ReadSubtitles is running.${OFF} It lives in the menu bar: ◉ reading, ◎ idle.

    ${BOLD}Control+Option+Command+S${OFF}   start and stop reading
    ${BOLD}Control+Option+Command+R${OFF}   draw a new subtitle region
    ${BOLD}Control+Option+Command+Q${OFF}   quit

    Start a film, then press ${BOLD}Control+Option+Command+S${OFF}. Every change is
    spoken aloud, so nothing needs to be read off the screen.

    ${DIM}Voices, speed and the subtitle region are in the menu bar.
    For the best quality, add an ElevenLabs voice: see the README.${OFF}

INFO
speak "Read Subtitles is installed and running. Press control option command S to start reading."
