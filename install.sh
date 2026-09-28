#!/bin/sh
# PLOMID install script.
#
# Installs the plomid-server binary for macOS or Linux from GitHub Releases:
#
#   curl -fsSL https://plomid.in/install.sh | sh
#
# Read this file before you run it.
#
# Release wiring: asset filenames below mirror `.github/workflows/release.yml`
# (macos-arm64, macos-x64, linux-x64, linux-arm64 jobs). The tarballs each
# carry a top-level `plomid-server` binary. If a filename changes there,
# change it here too (a static shell script cannot import YAML).
#
# Interactive: when run on a terminal this script shows the PLOMID banner,
# animates long steps, and confirms the version + install directory before
# downloading. Piped runs (`curl | sh`) without a TTY skip prompts safely.
# Pass --yes to skip prompts, --no-color to disable colors.
#
# Options (environment or flags):
#   PLOMID_VERSION=x.y.z  Pin a version (default: latest release).
#                         Accepts "1.2.3" or "v1.2.3".
#   PLOMID_INSTALL_DIR=DIR
#                         Install directory (default: first writable of
#                         /usr/local/bin, $HOME/.local/bin).
#   PLOMID_REPO=owner/repo
#                         Override the GitHub repo (default: plomid/plomid).
#   NO_COLOR=1            Disable colors even on a terminal.
#
#   sh install.sh --help             Show this help.
#   sh install.sh --dry-run          Print what would be downloaded, change nothing.
#   sh install.sh --yes              Skip confirmation prompts.
#   sh install.sh -v v0.1.0-beta.2   Pin a version (short flag).
#   sh install.sh --list             List all available release versions.
#   sh install.sh -v v0.1.0-beta.2 --dir ~/.local/bin
#
# Exit codes: 0 installed (or dry-run), 1 usage/error, 2 unsupported platform.

set -eu

REPO="${PLOMID_REPO:-plomid/plomid}"
VERSION="${PLOMID_VERSION:-latest}"
INSTALL_DIR="${PLOMID_INSTALL_DIR:-}"
DRY_RUN=0
ASSUME_YES=0
NO_COLOR="${NO_COLOR:-0}"

for _arg in "$@"; do
  case "$_arg" in
    --no-color) NO_COLOR=1 ;;
  esac
done

if [ "$NO_COLOR" = "1" ] || [ "${TERM:-dumb}" = "dumb" ] || [ ! -t 1 ]; then
  _C_ACCENT=""; _C_GREEN=""; _C_YELLOW=""; _C_DIM=""; _C_BOLD=""; _C_RESET=""
else
  _C_ACCENT="[38;2;226;104;60m"
  _C_GREEN="[32m"
  _C_YELLOW="[33m"
  _C_DIM="[2m"
  _C_BOLD="[1m"
  _C_RESET="[0m"
fi

log() { printf '%s\n' "$*"; }
warn() { printf '%splomid-install: %s%s\n' "$_C_YELLOW" "$*" "$_C_RESET" >&2; }
die() { printf '%splomid-install: %s%s\n' "$_C_YELLOW" "$*" "$_C_RESET" >&2; exit 1; }
step() { printf '%s›%s %s\n' "$_C_ACCENT" "$_C_RESET" "$*"; }
ok() { printf '%s✓%s %s\n' "$_C_GREEN" "$_C_RESET" "$*"; }

banner() {
  if [ -t 1 ] && [ "$NO_COLOR" != "1" ]; then
    printf '\n%s  ____  _     ___  __  __ ___ ____%s\n' "$_C_ACCENT" "$_C_RESET"
    printf '%s |  _ \| |   / _ \|  \/  |_ _|  _ \%s\n' "$_C_ACCENT" "$_C_RESET"
    printf '%s | |_) | |  | | | | |\/| || || | | |%s\n' "$_C_ACCENT" "$_C_RESET"
    printf '%s |  __/| |__| |_| | |  | || || |_| |%s\n' "$_C_ACCENT" "$_C_RESET"
    printf '%s |_|   |_____\___/|_|  |_|___|____/%s\n' "$_C_ACCENT" "$_C_RESET"
    printf '%s   Platform for Modern Intelligence and Data%s\n' "$_C_DIM" "$_C_RESET"
    printf '%s   autonomous storage · pg-wire compatible%s\n' "$_C_DIM" "$_C_RESET"
    printf '%s   github  : https://github.com/%s%s\n' "$_C_DIM" "$REPO" "$_C_RESET"
    printf '%s   website : https://plomid.in%s\n\n' "$_C_DIM" "$_C_RESET"
  else
    log "PLOMID installer"
  fi
}

usage() {
  sed -n '2,40p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -y|--yes|--non-interactive) ASSUME_YES=1; shift ;;
    --no-color) shift ;;
    -l|--list) LIST_MODE=1; shift ;;
    -v|--version) VERSION="${2:?-v needs a value}"; shift 2 ;;
    --version=*) VERSION="${1#--version=}"; shift ;;
    -v*) VERSION="${1#-v}"; shift ;;
    --dir) INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
    --dir=*) INSTALL_DIR="${1#--dir=}"; shift ;;
    --) shift; break ;;
    -*) die "unknown flag: $1 (see --help)" ;;
    *) break ;;
  esac
done
LIST_MODE="${LIST_MODE:-0}"

# --- spinner ------------------------------------------------------------
_spin_pid=""
start_spinner() {
  _spin_msg="$1"
  if [ ! -t 1 ] || [ "$NO_COLOR" = "1" ]; then
    printf '%s\n' "$_spin_msg"
    return 0
  fi
  (
    _i=0
    while :; do
      _i=$((_i + 1))
      case $((_i % 4)) in
        0) _f="|" ;; 1) _f="/" ;; 2) _f="-" ;; *) _f="\\" ;;
      esac
      printf '\r%s%s%s %s' "$_C_ACCENT" "$_f" "$_C_RESET" "$_spin_msg"
      sleep 0.1
    done
  ) &
  _spin_pid="$!"
}
stop_spinner() {
  if [ -n "$_spin_pid" ]; then
    kill "$_spin_pid" 2>/dev/null || true
    wait "$_spin_pid" 2>/dev/null || true
    _spin_pid=""
    printf '\r%*s\r' 60 ""
  fi
}
cleanup_spin() { stop_spinner; }
trap cleanup_spin EXIT INT TERM

have() { command -v "$1" >/dev/null 2>&1; }

# Interactive reads must come from the TTY: `curl | sh` occupies stdin.
tty_read() {
  _prompt="$1"
  _default="$2"
  if [ "$ASSUME_YES" = "1" ]; then printf '%s' "$_default"; return 0; fi
  if [ ! -t 0 ] && [ ! -r /dev/tty ]; then printf '%s' "$_default"; return 0; fi
  if [ -t 0 ]; then
    printf '%s' "$_prompt" >&2
    read -r _answer </dev/tty 2>/dev/null || _answer=""
    if [ -z "$_answer" ]; then printf '%s' "$_default"; else printf '%s' "$_answer"; fi
  else
    printf '%s' "$_prompt" >/dev/tty 2>/dev/null || true
    read -r _answer </dev/tty 2>/dev/null || _answer=""
    if [ -z "$_answer" ]; then printf '%s' "$_default"; else printf '%s' "$_answer"; fi
  fi
}

downloader=""
if have curl; then
  downloader="curl"
elif have wget; then
  downloader="wget"
else
  die "need curl or wget to download the release"
fi

fetch() {
  if [ "$downloader" = "curl" ]; then
    curl -fsSL --retry 3 --proto '=https' --tlsv1.2 "$1" -o "$2"
  else
    wget -q --tries=3 --https-only "$1" -O "$2"
  fi
}

api_get() {
  if [ "$downloader" = "curl" ]; then
    curl -fsSL --retry 3 "$1" 2>/dev/null || true
  else
    wget -qO- --tries=3 "$1" 2>/dev/null || true
  fi
}

if [ "$LIST_MODE" = "1" ]; then
  banner
  step "available PLOMID versions (${REPO}):"
  TAGS_JSON="$(api_get "https://api.github.com/repos/${REPO}/releases?per_page=30")"
  TAGS="$(printf '%s' "$TAGS_JSON" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  if [ -z "$TAGS" ]; then
    warn "no releases published yet for ${REPO}."
    log "  Cut one with:  make release-beta VERSION=0.1.0-beta.2"
    log "  Then re-run:   sh install.sh --list"
    log "  If your releases live elsewhere: PLOMID_REPO=owner/repo sh install.sh --list"
    trap - EXIT INT TERM 2>/dev/null || true
    exit 0
  fi
  _n=0
  for _t in $TAGS; do
    _n=$((_n + 1))
    if [ "$_n" = "1" ]; then
      printf '  %s* %s%s %s(latest)%s\n' "$_C_GREEN" "$_C_BOLD" "$_t" "$_C_DIM" "$_C_RESET"
    else
      printf '    %s\n' "$_t"
    fi
  done
  log ""
  log "  Install one with:  sh install.sh -v ${_C_BOLD}<tag>${_C_RESET}"
  trap - EXIT INT TERM 2>/dev/null || true
  exit 0
fi

banner

OS="$(uname -s 2>/dev/null || echo unknown)"
ARCH="$(uname -m 2>/dev/null || echo unknown)"

ASSET=""
case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64) ASSET="macos-arm64" ;;
      x86_64|amd64) ASSET="macos-x64" ;;
      *) die "unsupported macOS architecture: $ARCH" ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64|amd64) ASSET="linux-x64" ;;
      arm64|aarch64) ASSET="linux-arm64" ;;
      *) die "unsupported Linux architecture: $ARCH" ;;
    esac
    ;;
  MINGW*|MSYS*|CYGWIN*|Windows*)
    warn "this script does not install on Windows."
    warn "Download the signed installer instead:"
    warn "  https://github.com/${REPO}/releases/latest"
    exit 2
    ;;
  *)
    die "unsupported OS: $OS (macOS and Linux only; Windows uses the .exe on the releases page)"
    ;;
esac
ok "platform: ${OS} / ${ARCH} → ${ASSET}"

step "1/5 resolving version (${VERSION})..."
case "$VERSION" in
  latest|"")
    start_spinner "contacting github releases..."
    if [ "$downloader" = "curl" ]; then
      TAG_JSON="$(curl -fsSL --retry 3 "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)"
    else
      TAG_JSON="$(wget -qO- --tries=3 "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)"
    fi
    RESOLVED="$(printf '%s' "$TAG_JSON" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    if [ -z "$RESOLVED" ]; then
      stop_spinner
      warn "could not resolve latest tag (no releases yet, no network, or API limit)."
      warn "list versions with: sh install.sh --list"
      warn "if your releases live elsewhere: PLOMID_REPO=owner/repo sh install.sh --list"
      die "then install one with: sh install.sh -v <tag>"
    fi
    BASE="https://github.com/${REPO}/releases/latest/download" ;;
  v*) RESOLVED="$VERSION"; BASE="https://github.com/${REPO}/releases/download/${VERSION}" ;;
  *) RESOLVED="v${VERSION}"; BASE="https://github.com/${REPO}/releases/download/v${VERSION}" ;;
esac
stop_spinner
ok "version: ${RESOLVED}"

FILE="plomid-${RESOLVED}-${ASSET}.tar.gz"
URL="${BASE}/${FILE}"
SUM_URL="${BASE}/SHA256SUMS.txt"

pick_dir() {
  if [ -n "$INSTALL_DIR" ]; then printf '%s' "$INSTALL_DIR"; return; fi
  if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then printf '/usr/local/bin'; return; fi
  printf '%s' "$HOME/.local/bin"
}
DEST_DIR="$(pick_dir)"
BIN_DEST="${DEST_DIR}/plomid-server"

log ""
log "  ${_C_DIM}asset    :${_C_RESET} ${FILE}"
log "  ${_C_DIM}source   :${_C_RESET} ${URL}"
log "  ${_C_DIM}target   :${_C_RESET} ${BIN_DEST}"
log ""

if [ -t 0 ] || [ -r /dev/tty ]; then
  if [ "$ASSUME_YES" != "1" ]; then
    _confirm="$(tty_read "  Install plomid-server ${RESOLVED} to ${BIN_DEST}? [Y/n] " "Y")"
    case "$_confirm" in
      [yY]|"") ;;
      *) log "aborted."; exit 0 ;;
    esac
    if [ -z "$INSTALL_DIR" ]; then
      _alt="$(tty_read "  Directory (Enter to keep ${DEST_DIR}): " "")"
      if [ -n "$_alt" ]; then
        DEST_DIR="$_alt"
        BIN_DEST="${DEST_DIR}/plomid-server"
      fi
    fi
  fi
fi

if [ "$DRY_RUN" = "1" ]; then
  log "dry-run: nothing downloaded, nothing installed."
  trap - EXIT INT TERM
  exit 0
fi

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t plomid-install)"
cleanup() { rm -rf "$TMP"; stop_spinner; }
trap cleanup EXIT INT TERM
ARCHIVE="${TMP}/${FILE}"

step "2/5 downloading ${FILE}..."
start_spinner "downloading..."
if fetch "$URL" "$ARCHIVE"; then
  stop_spinner
  ok "downloaded ($(du -h "$ARCHIVE" 2>/dev/null | cut -f1 || echo '?'))"
else
  stop_spinner
  die "download failed: $URL"
fi

step "3/5 verifying checksum..."
start_spinner "sha256..."
SUM_FILE="${TMP}/SHA256SUMS.txt"
if fetch "$SUM_URL" "$SUM_FILE" 2>/dev/null; then
  EXPECTED="$(grep -F "  ${FILE}" "$SUM_FILE" 2>/dev/null | awk '{print $1}' | tr -d ' \r\n' || true)"
  if [ -z "$EXPECTED" ]; then
    stop_spinner
    die "checksum entry for ${FILE} not found in SHA256SUMS.txt"
  fi
  ACTUAL=""
  if have shasum; then
    ACTUAL="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
  elif have sha256sum; then
    ACTUAL="$(sha256sum "$ARCHIVE" | awk '{print $1}')"
  else
    stop_spinner
    die "no shasum/sha256sum found - cannot verify. Deleted, do not run it."
  fi
  stop_spinner
  if [ "$ACTUAL" = "$EXPECTED" ]; then
    ok "checksum ok (sha256)."
  else
    die "checksum mismatch - deleted, do not run it. Expected ${EXPECTED}, got ${ACTUAL}."
  fi
else
  stop_spinner
  die "could not download SHA256SUMS.txt - refusing unverified install."
fi

step "4/5 unpacking..."
tar -xzf "$ARCHIVE" -C "$TMP" || die "could not unpack $FILE"
SRC_BIN="${TMP}/plomid-server"
if [ ! -f "$SRC_BIN" ]; then
  die "archive did not contain a plomid-server binary"
fi
chmod +x "$SRC_BIN"
ok "unpacked."

step "5/5 installing to ${BIN_DEST}..."
mkdir -p "$DEST_DIR" || die "could not create $DEST_DIR"
if [ -w "$DEST_DIR" ]; then
  if have install; then
    install -m 0755 "$SRC_BIN" "$BIN_DEST"
  else
    cp "$SRC_BIN" "$BIN_DEST" && chmod 0755 "$BIN_DEST"
  fi
else
  warn "$DEST_DIR is not writable - retrying with sudo."
  have sudo || die "need sudo to write to $DEST_DIR, or set PLOMID_INSTALL_DIR to a writable directory"
  sudo mkdir -p "$DEST_DIR"
  if have install; then
    sudo install -m 0755 "$SRC_BIN" "$BIN_DEST"
  else
    sudo cp "$SRC_BIN" "$BIN_DEST" && sudo chmod 0755 "$BIN_DEST"
  fi
fi

if [ -x "$BIN_DEST" ]; then
  ok "installed to ${BIN_DEST}"
else
  die "copy finished but ${BIN_DEST} is not executable"
fi

case ":$PATH:" in
  *":${DEST_DIR}:"*) ;;
  *) warn "${DEST_DIR} is not on your PATH - add: export PATH=\"${DEST_DIR}:\$PATH\"" ;;
esac

if "$BIN_DEST" --version >/dev/null 2>&1; then
  _ver="$("$BIN_DEST" --version 2>/dev/null || true)"
  log "  ${_C_DIM}version  :${_C_RESET} ${_ver}"
fi

trap - EXIT INT TERM
cleanup
trap - EXIT INT TERM 2>/dev/null || true

printf '\n%s  ┌──────────────────────────────────────────────┐%s\n' "$_C_ACCENT" "$_C_RESET"
printf '%s  │%s  %sPLOMID %s ready%s                            %s│%s\n' \
  "$_C_ACCENT" "$_C_RESET" "$_C_GREEN" "$RESOLVED" "$_C_RESET" "$_C_ACCENT" "$_C_RESET"
printf '%s  └──────────────────────────────────────────────┘%s\n\n' "$_C_ACCENT" "$_C_RESET"
log "Next steps:"
log "  ${BIN_DEST} --data ./data --host 127.0.0.1 --port 5432"
log "  psql -h 127.0.0.1 -p 5432 -U plomid    # connect with any Postgres client"
log ""
log "Links:"
log "  website : https://plomid.in"
log "  github  : https://github.com/${REPO}"
log "  releases: https://github.com/${REPO}/releases"
log "  homebrew: brew install plomid/homebrew-plomid/plomid"
log ""
log "Prefer Docker?  docker run -d -p 5432:5432 plomid/plomid:latest"
