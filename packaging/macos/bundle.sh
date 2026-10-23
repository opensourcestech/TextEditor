#!/usr/bin/env bash
# Builds dist/TextEditor.app (one binary for Intel and Apple Silicon) and
# dist/TextEditor-<version>.dmg.
#
#   packaging/macos/bundle.sh             ad-hoc signed: runs on this Mac only
#   packaging/macos/bundle.sh --install   also copies the app to /Applications
#
# To share it with other people, sign it with a Developer ID certificate and
# have Apple notarize it, so Gatekeeper opens it without warnings:
#
#   SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
#   NOTARY_PROFILE=texteditor-notary packaging/macos/bundle.sh
#
# The notary profile is saved once, with an app-specific password, by:
#   xcrun notarytool store-credentials texteditor-notary --apple-id you@example.com --team-id TEAMID
set -euo pipefail

cd "$(dirname "$0")/../.."
here=packaging/macos
app=dist/TextEditor.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
dmg=dist/TextEditor-$version.dmg
sign_identity=${SIGN_IDENTITY:-}
notary_profile=${NOTARY_PROFILE:-}

if [[ -n "$notary_profile" && -z "$sign_identity" ]]; then
    echo "NOTARY_PROFILE needs SIGN_IDENTITY: Apple only notarizes Developer ID-signed apps." >&2
    exit 1
fi

# ----- Build for both architectures and merge them -----------------------
export MACOSX_DEPLOYMENT_TARGET=11.0 # matches LSMinimumSystemVersion
# Keep the builder's home folder (and so their username) out of the paths
# that panic messages embed in the binary. Separated by \x1f, so paths with
# spaces work.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"$'\x1f'"--remap-path-prefix=$PWD=."
targets=(x86_64-apple-darwin aarch64-apple-darwin)
binaries=()
for target in "${targets[@]}"; do
    rustup target list --installed | grep -qx "$target" || rustup target add "$target"
    cargo build --release --target "$target"
    binaries+=("target/$target/release/text_editor")
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/TextEditor" "${binaries[@]}"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
# The app icon, the same as the Windows app's.
cp "$here/AppIcon.icns" "$app/Contents/Resources/"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ----- Sign and notarize -------------------------------------------------
sign() {
    if [[ -n "$sign_identity" ]]; then
        # Hardened runtime and a secure timestamp are required for notarization.
        codesign --force --options runtime --timestamp --sign "$sign_identity" "$1"
    else
        codesign --force --sign - "$1"
    fi
}

notarize() {
    xcrun notarytool submit "$1" --keychain-profile "$notary_profile" --wait
}

sign "$app"
if [[ -n "$notary_profile" ]]; then
    ditto -c -k --keepParent "$app" "$work/TextEditor.zip"
    notarize "$work/TextEditor.zip"
    # Attach the ticket so the app also opens offline.
    xcrun stapler staple "$app"
fi

# ----- Disk image: the app plus a shortcut to drag it into Applications ----
mkdir "$work/dmg"
cp -R "$app" "$work/dmg/"
ln -s /Applications "$work/dmg/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "TextEditor $version" -srcfolder "$work/dmg" -ov -format UDZO "$dmg"
if [[ -n "$sign_identity" ]]; then
    sign "$dmg"
fi
if [[ -n "$notary_profile" ]]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
fi

# Tell Finder about the app's file types, for "Open With".
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app"

echo "Built $app and $dmg ($version, $(lipo -archs "$app/Contents/MacOS/TextEditor"))"
if [[ -z "$sign_identity" ]]; then
    echo "Signed ad-hoc: runs on this Mac. Set SIGN_IDENTITY and NOTARY_PROFILE to share it."
fi

if [[ "${1:-}" == "--install" ]]; then
    rm -rf /Applications/TextEditor.app
    cp -R "$app" /Applications/
    echo "Installed /Applications/TextEditor.app"
fi
