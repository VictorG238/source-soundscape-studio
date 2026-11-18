#!/bin/sh
set -eu
case "$1" in
  windows)
    cargo xwin build --locked --release --target x86_64-pc-windows-msvc
    src="$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/source-soundscape-studio.exe"
    name=SourceSoundscapeStudio.exe
    llvm-readobj --file-headers --coff-imports "$src"
    ;;
  linux)
    cargo build --locked --release
    src="$CARGO_TARGET_DIR/release/source-soundscape-studio"
    name=SourceSoundscapeStudio
    ;;
  *) exit 2 ;;
esac
mkdir -p dist
cp "$src" "dist/$name.tmp"
mv "dist/$name.tmp" "dist/$name"
