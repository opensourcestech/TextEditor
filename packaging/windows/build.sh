#!/usr/bin/env bash
# Builds dist/TextEditor.exe for 64-bit Windows, and
# dist/TextEditor-<version>-windows.zip to share it. Runs on macOS or Linux,
# with the MinGW-w64 cross-compiler (`brew install mingw-w64`).
#
#   packaging/windows/build.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
target=x86_64-pc-windows-gnu
exe=dist/TextEditor.exe
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
zip=dist/TextEditor-$version-windows.zip

if ! command -v x86_64-w64-mingw32-gcc > /dev/null; then
    echo "Needs the MinGW-w64 cross-compiler: brew install mingw-w64" >&2
    exit 1
fi

# ----- Build ---------------------------------------------------------------
# Keep the builder's home folder (and so their username) out of the paths
# that panic messages embed in the binary. Separated by \x1f, so paths with
# spaces work.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"$'\x1f'"--remap-path-prefix=$PWD=."
rustup target list --installed | grep -qx "$target" || rustup target add "$target"
cargo build --release --target "$target"

mkdir -p dist
cp "target/$target/release/text_editor.exe" "$exe"

# ----- Zip: what goes on the Releases page -----------------------------------
rm -f "$zip"
zip -j -q "$zip" "$exe"
