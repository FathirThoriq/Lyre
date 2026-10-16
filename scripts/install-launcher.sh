#!/bin/sh
# Lyre - user-level launcher install (no sudo).
#
# What it does:
#   1. Copies the lyre binary to ~/.local/bin/lyre and creates a
#      ~/.local/bin/lyr symlink (the short command).
#   2. For each shell you have (bash / zsh / fish), appends ONE
#      marked block to its rc file that puts ~/.local/bin on PATH
#      and defines the `lyr` alias. Re-running replaces the block;
#      it never duplicates.
#   3. --uninstall removes the marked blocks and both binaries.
#
# The binary is looked up next to this script's package directory
# first (./lyre, as shipped in the release tarball), then at
# ./target/release/lyre (a local cargo build).
set -eu

BIN_DIR="$HOME/.local/bin"
START_MARK='# >>> lyre >>>'
END_MARK='# <<< lyre <<<'

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PKG_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

# Remove any existing marked block from file $1 (no-op if absent).
strip_block() {
    f="$1"
    [ -f "$f" ] || return 0
    if grep -qF "$START_MARK" "$f"; then
        tmp="$f.lyre-tmp"
        awk -v s="$START_MARK" -v e="$END_MARK" '
            $0 == s { skip = 1; next }
            $0 == e { skip = 0; next }
            !skip { print }
        ' "$f" > "$tmp"
        mv "$tmp" "$f"
    fi
}

# Append the marked block for shell $2 (bash|zsh|fish) to rc $1,
# but only when that shell exists on this machine (its binary is on
# PATH) or its rc file already exists.
install_block() {
    rc="$1"
    shell_name="$2"
    if ! command -v "$shell_name" >/dev/null 2>&1 && [ ! -f "$rc" ]; then
        echo "  skip $rc ($shell_name not installed, no rc file)"
        return 0
    fi
    mkdir -p "$(dirname -- "$rc")"
    strip_block "$rc"
    {
        echo "$START_MARK"
        if [ "$shell_name" = "fish" ]; then
            echo 'set -gx PATH $HOME/.local/bin $PATH'
            echo 'alias lyr lyre'
        else
            echo 'export PATH="$HOME/.local/bin:$PATH"'
            echo "alias lyr='lyre'"
        fi
        echo "$END_MARK"
    } >> "$rc"
    echo "  updated $rc (PATH + lyr alias, one marked block)"
}

remove_block() {
    rc="$1"
    if [ -f "$rc" ] && grep -qF "$START_MARK" "$rc"; then
        strip_block "$rc"
        echo "  removed lyre block from $rc"
    fi
}

BASH_RC="$HOME/.bashrc"
ZSH_RC="$HOME/.zshrc"
FISH_RC="$HOME/.config/fish/config.fish"

if [ "${1:-}" = "--uninstall" ]; then
    echo "Uninstalling the Lyre launcher (user level)..."
    remove_block "$BASH_RC"
    remove_block "$ZSH_RC"
    remove_block "$FISH_RC"
    for f in "$BIN_DIR/lyre" "$BIN_DIR/lyr"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            rm -f "$f"
            echo "  removed $f"
        fi
    done
    echo "Done. (The lyre binary in the package directory was not touched.)"
    exit 0
fi

SRC=""
if [ -f "$PKG_DIR/lyre" ]; then
    SRC="$PKG_DIR/lyre"
elif [ -f "$PKG_DIR/target/release/lyre" ]; then
    SRC="$PKG_DIR/target/release/lyre"
else
    echo "Error: no lyre binary found at $PKG_DIR/lyre or $PKG_DIR/target/release/lyre" >&2
    echo "Build first (cargo build --release) or run this script from the extracted release package." >&2
    exit 1
fi

echo "Installing the Lyre launcher (user level, no sudo)..."
mkdir -p "$BIN_DIR"
cp "$SRC" "$BIN_DIR/lyre"
chmod +x "$BIN_DIR/lyre"
echo "  copied $SRC -> $BIN_DIR/lyre"
ln -sf "lyre" "$BIN_DIR/lyr"
echo "  symlink $BIN_DIR/lyr -> lyre"

echo "Shell setup:"
install_block "$BASH_RC" bash
install_block "$ZSH_RC" zsh
install_block "$FISH_RC" fish

echo ""
echo "Done. Open a new terminal (or source your rc file), then run: lyre  (or: lyr)"
echo "Uninstall later with: scripts/install-launcher.sh --uninstall"
