#!/usr/bin/env bash
# Put a built `shellrs` binary into ShellRS.app.
#
#   packaging/macos/bundle.sh <binary> <version> <build-number> <out-dir>
#
# The icon is assets/logo/shellrs.icns, or made from assets/logo/shellrs.png
# (1024×1024) when only that is there. Signing, notarizing and zipping are
# the release workflow's steps.
set -euo pipefail

binary=$1
version=$2
build=$3
out=$4

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
app="$out/ShellRS.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/shellrs"
chmod 755 "$app/Contents/MacOS/shellrs"
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" \
    "$here/Info.plist.in" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
# The Info.plist descriptions in each language of the interface.
for lproj in "$here"/*.lproj; do
    cp -R "$lproj" "$app/Contents/Resources/"
    plutil -lint "$app/Contents/Resources/$(basename "$lproj")/InfoPlist.strings" >/dev/null
done

icns="$root/assets/logo/shellrs.icns"
icon="$root/assets/logo/shellrs.png"
if [ -f "$icns" ]; then
    cp "$icns" "$app/Contents/Resources/ShellRS.icns"
elif [ -f "$icon" ]; then
    work=$(mktemp -d)
    iconset="$work/ShellRS.iconset"
    mkdir -p "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/ShellRS.icns"
    rm -rf "$work"
else
    echo "warning: no icon in assets/logo; the bundle has none" >&2
fi

echo "$app"
