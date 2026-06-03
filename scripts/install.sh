#!/usr/bin/env bash
#
# aatxe installer — one-shot `curl | sh` of the latest released binary.
#
# Usage (most common):
#
#   curl -fsSL https://raw.githubusercontent.com/enekos/aatxe/master/scripts/install.sh | sh
#
# Customisation knobs (env vars, all optional):
#
#   AATXE_VERSION   — version tag to install, e.g. v0.1.0. Defaults to
#                     "latest" which resolves the most recent GitHub
#                     Release.
#   AATXE_PREFIX    — install prefix. Defaults to $HOME/.local. The
#                     binary lands at $AATXE_PREFIX/bin/aatxe.
#   AATXE_REPO      — owner/name to pull releases from. Defaults to
#                     enekos/aatxe.
#   AATXE_NO_CHECK  — when "1", skip the sha256 verification step.
#
# Exit codes:
#   0 — installed.
#   1 — usage or environment error (missing curl/tar/uname).
#   2 — release asset not found for this host's OS/arch.
#   3 — sha256 mismatch.

set -euo pipefail

VERSION="${AATXE_VERSION:-latest}"
PREFIX="${AATXE_PREFIX:-$HOME/.local}"
REPO="${AATXE_REPO:-enekos/aatxe}"
NO_CHECK="${AATXE_NO_CHECK:-0}"

err() { echo "aatxe install: error: $*" >&2; }
log() { echo "aatxe install: $*" >&2; }

# Tool sanity. We rely on curl + tar + shasum/sha256sum + uname.
need() {
    command -v "$1" > /dev/null 2>&1 || { err "missing required tool: $1"; exit 1; }
}
need curl
need tar
need uname

OS=""
ARCH=""
case "$(uname -s)" in
    Linux)   OS="unknown-linux-gnu" ;;
    Darwin)  OS="apple-darwin" ;;
    *) err "unsupported OS: $(uname -s)"; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64|amd64)   ARCH="x86_64" ;;
    arm64|aarch64)  ARCH="aarch64" ;;
    *) err "unsupported arch: $(uname -m)"; exit 1 ;;
esac

TARGET="${ARCH}-${OS}"
ASSET="aatxe-${TARGET}.tar.gz"

# Resolve the tag if the user asked for "latest".
if [ "$VERSION" = "latest" ]; then
    log "resolving latest release for ${REPO}…"
    LATEST_JSON="$(curl -fsSL -H 'Accept: application/vnd.github+json' \
        "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)"
    if [ -z "$LATEST_JSON" ]; then
        err "could not fetch latest release for ${REPO}. Is the repo public + has a v* tag?"
        exit 2
    fi
    VERSION="$(printf '%s\n' "$LATEST_JSON" | sed -nE 's/.*"tag_name": *"([^"]+)".*/\1/p' | head -1)"
    if [ -z "$VERSION" ]; then
        err "could not parse latest tag_name."
        exit 2
    fi
    log "latest is ${VERSION}"
fi

BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
ARCHIVE_URL="${BASE_URL}/${ASSET}"
SHA_URL="${ARCHIVE_URL}.sha256"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

log "downloading ${ARCHIVE_URL}…"
if ! curl -fsSL -o "${TMP}/${ASSET}" "${ARCHIVE_URL}"; then
    err "no release asset for ${TARGET} at ${VERSION}. Open an issue + we'll add the matrix entry."
    exit 2
fi

if [ "$NO_CHECK" != "1" ]; then
    log "verifying sha256…"
    if ! curl -fsSL -o "${TMP}/${ASSET}.sha256" "${SHA_URL}"; then
        err "could not download checksum from ${SHA_URL}. Set AATXE_NO_CHECK=1 to skip."
        exit 3
    fi
    SHA_BIN="$(command -v sha256sum 2>/dev/null || command -v shasum)"
    if [ -z "$SHA_BIN" ]; then
        err "no sha256sum/shasum found. Set AATXE_NO_CHECK=1 to skip."
        exit 3
    fi
    EXPECTED="$(awk '{print $1}' "${TMP}/${ASSET}.sha256")"
    case "$SHA_BIN" in
        *shasum) GOT="$(shasum -a 256 "${TMP}/${ASSET}" | awk '{print $1}')" ;;
        *)        GOT="$(sha256sum     "${TMP}/${ASSET}" | awk '{print $1}')" ;;
    esac
    if [ "$EXPECTED" != "$GOT" ]; then
        err "sha256 mismatch: expected ${EXPECTED}, got ${GOT}."
        exit 3
    fi
fi

log "extracting…"
tar -xzf "${TMP}/${ASSET}" -C "$TMP"

DEST_DIR="${PREFIX}/bin"
mkdir -p "$DEST_DIR"
install -m 0755 "${TMP}/aatxe/aatxe" "${DEST_DIR}/aatxe"

log "installed aatxe ${VERSION} to ${DEST_DIR}/aatxe"

# Friendly PATH check.
case ":${PATH}:" in
    *":${DEST_DIR}:"*) ;;
    *)
        log "note: ${DEST_DIR} is not on your \$PATH yet. Add it with:"
        log "  echo 'export PATH=\"${DEST_DIR}:\$PATH\"' >> ~/.zshrc   # or ~/.bashrc"
        ;;
esac

"${DEST_DIR}/aatxe" --version || true
