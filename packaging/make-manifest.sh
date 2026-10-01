#!/usr/bin/env bash
# Write the signed update manifest of one release for one channel.
#
#   packaging/make-manifest.sh <version> <channel> <notes.md> <dist-dir> <out.json>
#
# <dist-dir> holds the release's packages under their published names.
# MINISIGN_KEY_FILE is the secret key; MINISIGN_PASSWORD its password, if it
# has one. SHELLRS_MIRROR_BASE, when set (e.g. the GitHub release download
# URL of a public repository), adds a second address for every package.
#
# The output is the envelope src/update/manifest.rs reads: the manifest's
# exact text and its minisign signature, whose trusted comment
# `shellrs-manifest <channel> <version>` the client checks. The format only
# ever gains fields; packaging/manifest.example.json shows it.
set -euo pipefail

version=$1
channel=$2
notes=$3
dist=$4
out=$5

base="https://dl.shellrs.com/releases/$version"
mirror=${SHELLRS_MIRROR_BASE:-}

mac="ShellRS-$version-macos-universal.app.zip"
dmg="ShellRS-$version-macos-universal.dmg"
win="ShellRS-$version-windows-x86_64-setup.exe"
linux="ShellRS-$version-linux-x86_64.AppImage"

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# The asset entry of one package: its addresses, size and SHA-256.
asset() {
    local file="$dist/$1"
    local size
    size=$(wc -c <"$file" | tr -d ' ')
    jq -n \
        --arg url "$base/$1" \
        --arg mirror "${mirror:+$mirror/$1}" \
        --argjson size "$size" \
        --arg sha256 "$(sha256 "$file")" \
        '{urls: ([$url] + (if $mirror == "" then [] else [$mirror] end)), size: $size, sha256: $sha256}'
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

jq -n -j \
    --arg version "$version" \
    --arg channel "$channel" \
    --arg published "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --rawfile notes "$notes" \
    --arg notes_url "https://shellrs.com/changelog#$version" \
    --argjson mac "$(asset "$mac")" \
    --argjson win "$(asset "$win")" \
    --argjson linux "$(asset "$linux")" \
    --arg dmg "$base/$dmg" \
    --arg win_url "$base/$win" \
    --arg linux_url "$base/$linux" \
    '{
        schema: 1,
        channel: $channel,
        version: $version,
        published_at: $published,
        minimum_version: null,
        rollout: 1.0,
        notes: $notes,
        notes_url: $notes_url,
        assets: {
            "macos-aarch64": $mac,
            "macos-x86_64": $mac,
            "windows-x86_64": $win,
            "linux-x86_64": $linux
        },
        installers: {
            "macos-aarch64": $dmg,
            "macos-x86_64": $dmg,
            "windows-x86_64": $win_url,
            "linux-x86_64": $linux_url
        }
    }' >"$work/manifest.json"

printf '%s\n' "${MINISIGN_PASSWORD:-}" | minisign -S \
    -s "$MINISIGN_KEY_FILE" \
    -m "$work/manifest.json" \
    -x "$work/manifest.json.minisig" \
    -t "shellrs-manifest $channel $version"

jq -n \
    --rawfile manifest "$work/manifest.json" \
    --rawfile signature "$work/manifest.json.minisig" \
    '{manifest: $manifest, signature: $signature}' >"$out"
echo "$out"
