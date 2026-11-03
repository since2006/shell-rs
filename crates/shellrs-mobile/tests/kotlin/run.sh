#!/bin/sh
# Builds Check.kt with the Kotlin interface scripts/build-android.sh made, and
# runs it on this computer's JVM against the build of the core for this
# computer it made beside.
#
# CLASSPATH holds the kotlin-stdlib, kotlinx-coroutines-core-jvm and JNA
# jars. KOTLINC is the compiler, `kotlinc` unless set.
set -eu
cd "$(dirname "$0")/../../../.."
out=target/mobile/kotlin-check
rm -rf "$out"
mkdir -p "$out"
${KOTLINC:-kotlinc} -nowarn -classpath "$CLASSPATH" -d "$out/check.jar" \
    target/mobile/android/kotlin/com/shellrs/core/*.kt \
    crates/shellrs-mobile/tests/kotlin/Check.kt
java -Djna.library.path=target/debug \
    -classpath "$out/check.jar:$CLASSPATH" CheckKt
