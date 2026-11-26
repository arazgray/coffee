# Coffee

Coffee is a native Rust player for video, music, and photos, using libmpv and FFmpeg. Its interface is dark and does not need a browser.

## Install

Get the package for your system from the [releases page](https://github.com/arazgray/coffee/releases). Coffee requires **libmpv 0.41 or newer**.

- **Ubuntu, Kubuntu, and Debian-based systems:** use the `.deb` on a release that provides libmpv 0.41 or newer. Older stock repositories may not supply a new enough version.
- **Fedora:** use the `.rpm` on Fedora 44 or newer. **Arch and other Linux distributions:** use the Linux `.tar.gz` with your distribution's libmpv, KDialog, and `xdg-utils`. The archive contains `bin/` and `share/` directories that can be installed under a user or system prefix.
- **Windows x64:** unzip the Windows archive. Install [MSYS2](https://www.msys2.org/) and run `pacman -S mingw-w64-ucrt-x86_64-mpv mingw-w64-ucrt-x86_64-libheif` in its **UCRT64** terminal. Launch `Coffee.exe`. Coffee looks for MSYS2 under `C:\msys64` by default, or under `MSYS2_ROOT` if set.
- **macOS:** run `brew install mpv libheif`, extract the zip for your CPU, and open `Coffee.app`. The app is not notarized. Coffee checks the standard Apple Silicon and Intel Homebrew prefixes.

To build from source, install Rust and run `cargo test --locked` and `cargo build --release --locked`. On Linux, `sh install.sh` installs the result for the current user. `coffee --smoke-test path/to/media` checks opening and decoding a file without creating a player window. `COFFEE_LIBMPV` can point to a different libmpv installation.

## Playback

- Drop media on the window to add it to the queue and play the first dropped item. **Open → Add files to queue** appends without interrupting playback.
- Open a photo to browse supported images in its folder with **Previous** and **Next**. Filenames are sorted naturally. A same-named `.MOV` companion adds a **Live** badge to the photo; click it to play the clip and automatically return to the still. The preview does not leave an extra playlist item.
- **Open Streams** uses the player's own dark URL dialog. It accepts HLS, DASH, and direct HTTP(S) URLs. Paste with **Ctrl+V** (or **Command+V** where supported).
- The default filters include common video, audio, and image formats. Supported images include JPEG, PNG/APNG, HEIC/HEIF, AVIF, JPEG XL, QOI, WebP (lossy, lossless, and animated), TIFF, GIF, BMP, and JPEG 2000. Animated WebP from a local file works on older FFmpeg builds too. Experimental WebP v2 is not supported.
- Full-resolution tiled HEIC/HEIF (common on iPhones) uses libheif with an HEVC decoder plugin when installed. The Debian package recommends `libheif-plugin-libde265`; Fedora users may need `libheif-freeworld` from RPM Fusion. Codec availability otherwise depends on the libmpv/FFmpeg build.
- The playlist supports `.coffee.json` and older `.kahve.json` files, plus `.m3u`/`.m3u8` import. Playlists refer to local paths and URLs; they don't copy media.
- Double-click the video to toggle fullscreen. Double-click open space on either control bar to maximize or restore. Right-click for playback actions and the current item's containing folder or playlist entry.
- The player and stream dialog are always dark. On KDE Plasma 6, Coffee adds a dark window-frame rule scoped to `coffee`. Other Linux compositors may control their own decorations. Coffee requests a dark frame on Windows and macOS.

## HDR and hardware decoding

Coffee uses libmpv's `gpu-next` renderer. On Wayland it tries Vulkan first, keeping video on the native renderer and the controls in an SDR overlay. **HDR** contains **Off (SDR only)**, **Automatic**, **Prefer native HDR**, **Video decoder**, and **Rendering quality**.

**Video info** shows the actual decoder, display target, and dropped frames. Choosing a GPU cannot accelerate a codec that it does not support. The default rendering mode switches to lighter scaling and static tone mapping for 4K at 50 fps or above. HDR presentation still depends on the GPU, driver, and display.

## Artwork and license

Coffee is licensed under [MIT](LICENSE). The bundled [Lucide](assets/LUCIDE-LICENSE) assets are converted to SDR vector paths at build time. `tools/sync-icons.mjs` refreshes them with `lucide` 1.80 installed; `tools/sync-logo.sh` refreshes the logo with ImageMagick and Potrace. Neither is needed to build or run from bundled assets.
