#!/bin/sh
# Higgsfield CLI installer.
#
# Installs `higgsfield` (primary) + `higgs` symlink (always).
# Tries to install `hf` shortcut unless taken (e.g. by huggingface CLI).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/higgsfield-ai/cli/main/install.sh | sh
#   curl -fsSL ... | sh -s -- --prefix=$HOME/.local
#   curl -fsSL ... | sh -s -- --tag v0.1.1
#   curl -fsSL ... | sh -s -- --no-hf            # skip hf shortcut
#   curl -fsSL ... | sh -s -- --hf               # force hf shortcut

set -e

REPO="higgsfield-ai/cli"
PREFIX="/usr/local"
TAG=""
INSTALL_HF=auto

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --prefix)   PREFIX="$2"; shift 2 ;;
    --tag=*)    TAG="${1#*=}"; shift ;;
    --tag)      TAG="$2"; shift 2 ;;
    --no-hf)    INSTALL_HF=no; shift ;;
    --hf)       INSTALL_HF=yes; shift ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  ARCH="amd64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *) echo "Unsupported arch: $ARCH" >&2; exit 1 ;;
esac
case "$OS" in
  darwin|linux) ;;
  *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ -z "$TAG" ]; then
  TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  if [ -z "$TAG" ]; then
    echo "Failed to determine latest release." >&2
    exit 1
  fi
fi
VER_NO_V="${TAG#v}"
TARBALL="hf_${VER_NO_V}_${OS}_${ARCH}.tar.gz"
URL="https://github.com/$REPO/releases/download/$TAG/$TARBALL"
echo "Downloading $URL"
curl -fsSL -o "$TMPDIR/$TARBALL" "$URL"

ARCHIVE="$(ls "$TMPDIR"/hf_*.tar.gz 2>/dev/null | head -n 1)"
if [ -z "$ARCHIVE" ]; then
  echo "No archive found." >&2
  exit 1
fi
tar -xzf "$ARCHIVE" -C "$TMPDIR"

BIN_DIR="$PREFIX/bin"
if [ ! -d "$BIN_DIR" ]; then
  mkdir -p "$BIN_DIR" 2>/dev/null || sudo mkdir -p "$BIN_DIR"
fi

run() {
  if [ -w "$BIN_DIR" ]; then "$@"; else sudo "$@"; fi
}

# Primary binary: higgsfield
run install -m 0755 "$TMPDIR/hf" "$BIN_DIR/higgsfield"
[ "$OS" = "darwin" ] && run xattr -d com.apple.quarantine "$BIN_DIR/higgsfield" 2>/dev/null || true
cat > "$TMPDIR/higgsfield.install.json" <<EOF
{
  "install_method": "curl",
  "prefix": "$PREFIX",
  "bin": "$BIN_DIR/higgsfield",
  "version": "$VER_NO_V"
}
EOF
run install -m 0644 "$TMPDIR/higgsfield.install.json" "$BIN_DIR/higgsfield.install.json"

# higgs symlink (always)
run ln -sf "$BIN_DIR/higgsfield" "$BIN_DIR/higgs"

# hf shortcut (optional)
HF_INSTALLED=no
if [ "$INSTALL_HF" = "no" ]; then
  echo "Skipping 'hf' shortcut (--no-hf)."
elif [ "$INSTALL_HF" = "yes" ]; then
  run ln -sf "$BIN_DIR/higgsfield" "$BIN_DIR/hf"
  HF_INSTALLED=yes
else
  if command -v hf >/dev/null 2>&1; then
    EXISTING="$(command -v hf)"
    if [ "$EXISTING" = "$BIN_DIR/hf" ]; then
      run ln -sf "$BIN_DIR/higgsfield" "$BIN_DIR/hf"
      HF_INSTALLED=yes
    else
      echo "Skipping 'hf' shortcut: $EXISTING already in PATH (huggingface or other tool)."
      echo "Force with --hf if you want to override."
    fi
  else
    run ln -sf "$BIN_DIR/higgsfield" "$BIN_DIR/hf"
    HF_INSTALLED=yes
  fi
fi

echo "Installed: $($BIN_DIR/higgsfield version)"
if [ "$HF_INSTALLED" = "yes" ]; then
  echo "Bins: higgsfield, higgs, hf"
else
  echo "Bins: higgsfield, higgs"
fi
