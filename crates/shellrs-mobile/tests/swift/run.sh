#!/bin/sh
# Builds main.swift with the Swift interface scripts/build-ios.sh made,
# against the build of the core for this Mac it made beside, and runs it.
set -eu
cd "$(dirname "$0")/../../../.."
ios=target/mobile/ios
out=target/mobile/swift-check
rm -rf "$out"
mkdir -p "$out"
swiftc -swift-version 6 -module-name ShellRSCore -parse-as-library \
    -emit-library -emit-module -emit-module-path "$out/ShellRSCore.swiftmodule" \
    -I "$ios/include" -L target/debug -lshellrs_mobile \
    "$ios/Sources/ShellRSCore.swift" -o "$out/libShellRSCore.dylib"
swiftc -swift-version 6 -I "$out" -I "$ios/include" \
    -L "$out" -lShellRSCore -L target/debug -lshellrs_mobile \
    -Xlinker -rpath -Xlinker "$PWD/$out" -Xlinker -rpath -Xlinker "$PWD/target/debug" \
    crates/shellrs-mobile/tests/swift/main.swift -o "$out/check"
"$out/check"
