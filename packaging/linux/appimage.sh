#!/usr/bin/env bash
# Put a built `shellrs` binary into an AppImage.
#
#   packaging/linux/appimage.sh <binary> <version> <out-dir>
#
# Needs appimagetool on the PATH (or APPIMAGETOOL), which uses the static
# type2 runtime: the AppImage runs without libfuse2, which Ubuntu 22.04 and
# later no longer install. The graphics stack (Vulkan, Mesa, Wayland, X) is
# the host's and is not bundled. The icon is assets/logo/shellrs.png.
set -euo pipefail

binary=$1
version=$2
out=$3

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tool=${APPIMAGETOOL:-appimagetool}

work=$(mktemp -d)
appdir="$work/ShellRS.AppDir"
mkdir -p "$appdir/usr/bin"
cp "$binary" "$appdir/usr/bin/shellrs"
chmod 755 "$appdir/usr/bin/shellrs"
cp "$here/shellrs.desktop" "$appdir/shellrs.desktop"
icon="$root/assets/logo/shellrs.png"
if [ ! -f "$icon" ]; then
    echo "error: $icon is missing; an AppImage needs an icon" >&2
    exit 1
fi
cp "$icon" "$appdir/shellrs.png"
# The executable itself is the entry point; the restart after an update
# starts $APPIMAGE again, so nothing here needs to survive it.
ln -s usr/bin/shellrs "$appdir/AppRun"

mkdir -p "$out"
target="$out/ShellRS-$version-linux-x86_64.AppImage"
ARCH=x86_64 "$tool" --no-appstream "$appdir" "$target"
rm -rf "$work"
echo "$target"
