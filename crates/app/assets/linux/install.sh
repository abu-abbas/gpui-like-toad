#!/bin/sh
# Kilat: install for the current user (no sudo needed).
#
#   sh install.sh             install or update
#   sh install.sh --uninstall remove it again
#
# Copies the binary to ~/.local/bin, plus the launcher entry and icons to
# ~/.local/share, so the app shows up with its icon in the app grid, dock
# and taskbar. Saved connections live elsewhere and are never touched.
#
# Kept non-executable in the tarball on purpose (run it with `sh`): the
# in-app updater picks the archive's executable file as the new binary.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
app=gpui-like-toad
sizes="16 32 64 128 256 512"

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 &&
        update-desktop-database -q "$data_dir/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 &&
        gtk-update-icon-cache -q -t -f "$data_dir/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin_dir/$app" "$data_dir/applications/$app.desktop"
    for s in $sizes; do
        rm -f "$data_dir/icons/hicolor/${s}x${s}/apps/$app.png"
    done
    refresh
    echo "Kilat removed."
    exit 0
fi

mkdir -p "$bin_dir" "$data_dir/applications"
install -m 755 "$here/$app" "$bin_dir/$app"
for s in $sizes; do
    mkdir -p "$data_dir/icons/hicolor/${s}x${s}/apps"
    install -m 644 "$here/share/icons/hicolor/${s}x${s}/apps/$app.png" \
        "$data_dir/icons/hicolor/${s}x${s}/apps/$app.png"
done
# Absolute Exec path: launchers do not always have ~/.local/bin on PATH.
sed "s|^Exec=.*|Exec=$bin_dir/$app|" \
    "$here/share/applications/$app.desktop" >"$data_dir/applications/$app.desktop"
chmod 644 "$data_dir/applications/$app.desktop"
refresh

echo "Kilat installed: $bin_dir/$app"
echo "Find it in the app menu (it may take a moment to appear)."
