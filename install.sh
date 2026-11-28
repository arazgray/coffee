#!/bin/sh
set -eu

cd "$(dirname "$0")"
prefix="${COFFEE_PREFIX:-${KAHVE_PREFIX:-$HOME/.local}}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
icons="$data/icons/hicolor/512x512/apps"
launcher="$apps/coffee.desktop"
binary="$prefix/bin/coffee"

cargo build --release --locked
install -Dm755 target/release/coffee "$binary"
install -Dm644 assets/icon.png "$icons/coffee.png"
install -Dm644 assets/LUCIDE-LICENSE "$data/coffee/LUCIDE-LICENSE"
mkdir -p "$apps"
cat > "$launcher" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=Coffee
GenericName=Media Player
Comment=Native Rust video, photo and music player
Exec="$binary" %U
Icon=coffee
Terminal=false
StartupNotify=false
StartupWMClass=coffee
Categories=AudioVideo;Audio;Video;Player;
MimeType=video/mp4;video/webm;video/x-matroska;video/quicktime;video/mpeg;video/ogg;video/mp2t;video/x-msvideo;audio/mpeg;audio/mp4;audio/flac;audio/ogg;audio/vnd.wave;audio/x-wav;audio/aac;audio/opus;audio/x-opus+ogg;image/jpeg;image/png;image/apng;image/webp;image/gif;image/avif;image/bmp;image/tiff;image/vnd.adobe.photoshop;image/svg+xml;image/heic;image/heif;image/jxl;image/qoi;image/jp2;
EOF
if command -v desktop-file-validate >/dev/null 2>&1; then desktop-file-validate "$launcher"; fi
legacy="$apps/kahve-native.desktop"
if [ -f "$legacy" ] && grep -Fxq 'Name=Kahve Player' "$legacy" &&
    grep -Fxq "Exec=\"$prefix/bin/kahve-native\" %U" "$legacy"; then
    rm -f -- "$legacy"
fi
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$apps"; fi
printf 'Installed %s and %s\n' "$binary" "$launcher"
