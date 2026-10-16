#!/bin/sh
# Lyre - install runtime dependencies: mpv + yt-dlp + cava
# Detects the distro (Fedora dnf / Debian apt / Arch pacman),
# installs what is missing, then prints the versions. Safe to run
# repeatedly (idempotent): already-installed packages are skipped by
# the package manager itself. cava powers the sidebar visualizer.
set -e

echo "Lyre: installing dependencies (mpv + yt-dlp + cava)..."

if [ -f /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    DISTRO_ID="${ID:-}"
    DISTRO_LIKE="${ID_LIKE:-}"
else
    DISTRO_ID=""
    DISTRO_LIKE=""
fi

case "$DISTRO_ID $DISTRO_LIKE" in
    *fedora*|*rhel*|*centos*)
        echo "Distro detected: Fedora/RHEL - using dnf."
        sudo dnf install -y mpv yt-dlp cava
        ;;
    *debian*|*ubuntu*)
        echo "Distro detected: Debian/Ubuntu - using apt."
        sudo apt update
        sudo apt install -y mpv yt-dlp cava
        ;;
    *arch*|*manjaro*|*endeavouros*)
        echo "Distro detected: Arch - using pacman."
        sudo pacman -S --needed --noconfirm mpv yt-dlp cava
        ;;
    *)
        echo "Distro not recognized automatically. Install manually, one of:"
        echo "  Fedora : sudo dnf install mpv yt-dlp cava"
        echo "  Debian : sudo apt install mpv yt-dlp cava"
        echo "  Arch   : sudo pacman -S mpv yt-dlp cava"
        exit 1
        ;;
esac

echo ""
echo "Installed versions:"
mpv --version | head -n 1 || true
yt-dlp --version || true
cava --version 2>/dev/null | head -n 1 || true
echo "Done. Run 'lyre' and press Enter on a track to play it."
