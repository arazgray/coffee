#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
version="$(awk -F '"' '/^version = / {print $2; exit}' Cargo.toml)"
arch="$(uname -m)"
export COFFEE_PROJECT="$PWD"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p dist "$work/pkg/DEBIAN"
package_root="$work/pkg"

install -Dm755 target/release/coffee "$package_root/usr/bin/coffee"
install -Dm644 assets/coffee.desktop "$package_root/usr/share/applications/coffee.desktop"
install -Dm644 assets/icon.png "$package_root/usr/share/icons/hicolor/512x512/apps/coffee.png"
install -Dm644 LICENSE "$package_root/usr/share/doc/coffee/copyright"
install -Dm644 assets/LUCIDE-LICENSE "$package_root/usr/share/doc/coffee/LUCIDE-LICENSE"
deb_arch="$(dpkg --print-architecture)"
cat > "$package_root/DEBIAN/control" <<EOF
Package: coffee-media-player
Version: $version
Section: video
Priority: optional
Architecture: $deb_arch
Depends: libc6 (>= 2.39), libmpv2 (>= 0.41), kdialog, xdg-utils
Recommends: libheif1, libheif-plugin-libde265
Maintainer: Araz Gray
Homepage: https://github.com/arazgray/coffee
Description: Coffee native media player
 Video, photo, and music playback with libmpv.
EOF
dpkg-deb --root-owner-group --build "$package_root" "dist/coffee-${version}-${deb_arch}.deb"

portable="$work/coffee-linux-$arch"
mkdir -p "$portable"
cp -a "$package_root/usr/." "$portable/"
cp README.md "$portable/"
tar -czf "dist/coffee-${version}-linux-${arch}.tar.gz" -C "$work" "coffee-linux-$arch"

if command -v rpmbuild >/dev/null 2>&1; then
    rpmbuild -bb --define "_topdir $work/rpm" --define "coffee_version $version" packaging/coffee.spec
    find "$work/rpm/RPMS" -name '*.rpm' -exec cp {} dist/ \;
fi
