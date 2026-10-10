#!/bin/sh
# Build the foreground helper dex. Needs javac and the Android SDK's d8; both are absent
# from a plain checkout, so this is deliberately NOT wired into build.sh — it is run by
# hand while the helper is a work in progress (see README.md for where it stands).
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
ANDROID_JAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-36/android.jar}"
D8="${D8:-$HOME/Android/Sdk/build-tools/36.1.0/d8}"
rm -rf "$DIR/build"
mkdir -p "$DIR/build/classes"
javac --release 8 -nowarn -d "$DIR/build/classes" "$DIR/ForegroundHelper.java" "$DIR/FgProbe.java"
"$D8" --release --lib "$ANDROID_JAR" --output "$DIR/build/foreground.jar" "$DIR/build/classes/ForegroundHelper.class"
"$D8" --release --lib "$ANDROID_JAR" --output "$DIR/build/probe.jar" "$DIR/build/classes/FgProbe.class"
echo "built: build/foreground.jar build/probe.jar"
