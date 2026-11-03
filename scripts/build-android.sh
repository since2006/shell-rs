#!/bin/sh
# Builds the mobile core for the Android app (see
# crates/shellrs-mobile/README.md):
#
#   target/mobile/android/jniLibs/<abi>/libshellrs_mobile.so  the library, for
#                                     64-bit ARM phones and x86_64 emulators
#   target/mobile/android/kotlin/  its Kotlin interface, which needs JNA
#
# Needs the Android NDK r28 or later (ANDROID_NDK_HOME, or the newest in the
# Android SDK), `cargo install cargo-ndk` and
# `rustup target add aarch64-linux-android x86_64-linux-android`.
# ANDROID_PLATFORM sets the oldest Android it runs on, API level 24 unless set.
set -eu
cd "$(dirname "$0")/.."
out=target/mobile/android

rm -rf "$out"
cargo ndk --platform "${ANDROID_PLATFORM:-24}" -t arm64-v8a -t x86_64 -o "$out/jniLibs" \
    build --locked --release -p shellrs-mobile --lib

# The interface is read from what the library says about itself, in a build
# for this computer.
cargo build --locked -p shellrs-mobile --features bindgen
case "$(uname)" in
    Darwin) library=target/debug/libshellrs_mobile.dylib ;;
    *) library=target/debug/libshellrs_mobile.so ;;
esac
target/debug/uniffi-bindgen generate --no-format --language kotlin \
    --library "$library" --out-dir "$out/kotlin"
# What the generated interface cannot say: Android's certificate checks.
cp crates/shellrs-mobile/android/Tls.kt "$out/kotlin/com/shellrs/core/"
