Name: coffee-media-player
Version: %{coffee_version}
Release: 1
Summary: Coffee native media player
License: MIT AND ISC
URL: https://github.com/arazgray/coffee
Requires: mpv-libs >= 0.41
Requires: kdialog, xdg-utils
Recommends: libheif
%global debug_package %{nil}

%description
Video, photo, and music playback with libmpv.

%install
install -Dm755 "$COFFEE_PROJECT/target/release/coffee" "%{buildroot}%{_bindir}/coffee"
install -Dm644 "$COFFEE_PROJECT/assets/coffee.desktop" "%{buildroot}%{_datadir}/applications/coffee.desktop"
install -Dm644 "$COFFEE_PROJECT/assets/icon.png" "%{buildroot}%{_datadir}/icons/hicolor/512x512/apps/coffee.png"
install -Dm644 "$COFFEE_PROJECT/LICENSE" "%{buildroot}%{_licensedir}/coffee-media-player/LICENSE"
install -Dm644 "$COFFEE_PROJECT/assets/LUCIDE-LICENSE" "%{buildroot}%{_licensedir}/coffee-media-player/LUCIDE-LICENSE"

%files
%{_bindir}/coffee
%{_datadir}/applications/coffee.desktop
%{_datadir}/icons/hicolor/512x512/apps/coffee.png
%license %{_licensedir}/coffee-media-player
