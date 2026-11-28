#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
version="$(awk -F '"' '/^version = / {print $2; exit}' Cargo.toml)"
arch="$(uname -m)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
app="$work/Coffee.app/Contents"
mkdir -p dist "$app/MacOS" "$app/Resources" "$work/Coffee.iconset"
install -m755 target/release/coffee "$app/MacOS/Coffee"
cp LICENSE assets/LUCIDE-LICENSE "$app/Resources/"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/icon.png --out "$work/Coffee.iconset/icon_${size}x${size}.png" >/dev/null
    double="$((size * 2))"
    sips -z "$double" "$double" assets/icon.png --out "$work/Coffee.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/Coffee.iconset" -o "$app/Resources/Coffee.icns"
cat > "$app/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Coffee</string>
    <key>CFBundleDisplayName</key><string>Coffee</string>
    <key>CFBundleIdentifier</key><string>com.arazgray.coffee</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>Coffee</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleIconFile</key><string>Coffee</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Media</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.audio</string>
                <string>public.movie</string>
                <string>public.video</string>
                <string>public.image</string>
                <string>com.adobe.photoshop-image</string>
                <string>public.svg-image</string>
                <string>org.matroska.mkv</string>
                <string>org.webmproject.webm</string>
            </array>
        </dict>
    </array>
    <!-- Tell libmpv to keep the bundle icon instead of replacing it. -->
    <key>LSEnvironment</key>
    <dict>
        <key>MPVBUNDLE</key><string>true</string>
    </dict>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSRequiresAquaSystemAppearance</key><false/>
</dict>
</plist>
EOF
plutil -lint "$app/Info.plist"
codesign --force --sign - "$work/Coffee.app"
ditto -c -k --keepParent "$work/Coffee.app" "dist/coffee-${version}-macos-${arch}.zip"
