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
- Open a photo to browse nearby images with **Previous** and **Next**. Photos switch without a fade or hiding the controls. Coffee preloads five photos on each side and their Live clips in the background, prioritizing the current Live clip. The RAM cache is bounded at 256 MiB (128 MiB per file); uncached files still open normally.
- A same-named `.MOV` companion adds a **Live** badge. Click it to play the clip and automatically return to the still without adding a playlist item. Live Photo transitions blend directly into the next frame over 150 ms.
- **Slideshow**, beside **Live** in the playback area, opens a dropdown with optional music and a **Play** button. Play enters fullscreen and advances every five seconds, looping through the folder. Press **Esc** to stop the slideshow and its music.
- The photo bar has **Fit** (show the entire image), **Fill** (crop to fill the window), and **100%** (one image pixel per display pixel). The selected zoom carries over to the next photo.
- **Open Streams** uses the player's own dark URL dialog. It accepts HLS, DASH, and direct HTTP(S) URLs. Paste with **Ctrl+V** (or **Command+V** where supported).
- The playlist supports `.coffee.json` and older `.kahve.json` files, plus `.m3u`/`.m3u8` import. Playlists refer to local paths and URLs; they don't copy media.
- Double-click a photo or video to toggle fullscreen. Double-click open space on either control bar to maximize or restore. Right-click for playback actions and the current item's containing folder or playlist entry.
- The player and stream dialog are always dark. On KDE Plasma 6, Coffee adds a dark window-frame rule scoped to `coffee`. Other Linux compositors may control their own decorations. Coffee requests a dark frame on Windows and macOS.

## Supported media

Coffee uses the demuxers and decoders in its installed libmpv/FFmpeg build. The lists below cover Coffee’s media filters and commonly supported codecs. Use **All files** for other extensions. Container support does not guarantee support for every codec or profile inside it; hardware decoding also depends on the GPU and driver. See FFmpeg’s [full format and codec reference](https://ffmpeg.org/general.html#Supported-File-Formats_002c-Codecs-or-Features) for the broader list.

### Video

| Format / container | Extensions |
| --- | --- |
| MPEG-4 / QuickTime | `.mp4`, `.m4v`, `.mov`, `.qt`, `.3gp`, `.3g2` |
| Matroska / WebM / Ogg | `.mkv`, `.webm`, `.ogv` |
| Audio Video Interleave | `.avi` |
| MPEG video and program streams / DVD | `.mpeg`, `.mpg`, `.m1v`, `.m2v`, `.vob` |
| MPEG transport streams / AVCHD | `.ts`, `.m2t`, `.m2ts`, `.mts` |
| Windows Media / ASF and Flash Video | `.wmv`, `.asf`, `.flv`, `.f4v` |
| Broadcast and legacy formats | `.mxf`, `.dv`, `.rm`, `.rmvb`, `.nut` |

**Video codecs:** H.264/AVC, H.265/HEVC, AV1, VP9, VP8, VP6, MPEG-1 Video, MPEG-2 Video, MPEG-4 Part 2 (including DivX/Xvid), H.261, H.263, Theora, VC-1, WMV7/8/9, Sorenson, RealVideo, Motion JPEG, DV, Apple ProRes, DNxHD/DNxHR, FFV1, CineForm, HuffYUV, UT Video, Hap, and raw/uncompressed video. Newer or optional decoders such as H.266/VVC depend on the bundled FFmpeg version and build options.

**Streams:** HLS (`.m3u8`), MPEG-DASH (`.mpd`), and direct HTTP(S) media URLs.

### Photos

- **JPEG** `.jpg`, `.jpeg`, `.jfif`, `.jpe`; **PNG/APNG** `.png`, `.apng`; **GIF** `.gif`; **WebP** `.webp` (lossy, lossless, transparency, and animation).
- **AVIF** `.avif`; **HEIC/HEIF** `.heic`, `.heif`, `.heics`, `.heifs`; **JPEG XL** `.jxl`; **JPEG 2000** `.jp2`, `.j2k`, `.j2c`; **QOI** `.qoi`.
- **TIFF** `.tif`, `.tiff`; **BMP** `.bmp`; **ICO** `.ico`; **Targa** `.tga`; **DDS** `.dds`; **Radiance HDR** `.hdr`; **OpenEXR** `.exr`; **Netpbm** `.pbm`, `.pgm`, `.ppm`, `.pam`.

Full-resolution tiled HEIC/HEIF (common on iPhones) uses libheif with an HEVC decoder. The Debian package recommends `libheif-plugin-libde265`; Fedora users may need `libheif-freeworld` from RPM Fusion. Local animated WebP works on older FFmpeg builds too. Experimental WebP v2 is not supported.

### Music

| Format / codec | Extensions |
| --- | --- |
| MPEG audio layers II and III | `.mp2`, `.mp3` |
| AAC (including AAC-LC / HE-AAC) | `.aac`, `.adts`, `.m4a`, `.m4b` |
| Apple Lossless | `.alac`, `.m4a`, `.m4b`, `.caf` |
| Lossless audio (FLAC, Monkey’s Audio, WavPack, True Audio) | `.flac`, `.ape`, `.wv`, `.tta` |
| PCM and other audio in WAV, AIFF, CAF and AU | `.wav`, `.wave`, `.aif`, `.aiff`, `.aifc`, `.caf`, `.au` |
| Vorbis and Opus | `.ogg`, `.oga`, `.opus` (also `.webm`, `.mka`) |
| Windows Media Audio | `.wma`, `.asf` |
| Dolby Digital / Digital Plus, DTS, and AMR | `.ac3`, `.eac3`, `.dts`, `.amr` |
| Matroska Audio | `.mka` |

Many of the same audio codecs also play as tracks inside video containers, including AAC, AC-3, E-AC-3, DTS, TrueHD, FLAC, Opus, Vorbis, and PCM. Audio passthrough and multi-channel output depend on the audio hardware and system configuration.

## HDR and hardware decoding

Coffee uses libmpv's `gpu-next` renderer. On Wayland it tries Vulkan first, keeping video on the native renderer and the controls in an SDR overlay. **HDR** contains **Off (SDR only)**, **Automatic**, **Prefer native HDR**, **Video decoder**, and **Rendering quality**.

**Video info** shows the actual decoder, display target, and dropped frames. Choosing a GPU cannot accelerate a codec that it does not support. The default rendering mode switches to lighter scaling and static tone mapping for 4K at 50 fps or above. HDR presentation still depends on the GPU, driver, and display.

## Artwork and license

Coffee is licensed under [MIT](LICENSE). The bundled [Lucide](assets/LUCIDE-LICENSE) assets are converted to SDR vector paths at build time. `tools/sync-icons.mjs` refreshes them with `lucide` 1.80 installed; `tools/sync-logo.sh` refreshes the logo with ImageMagick and Potrace. Neither is needed to build or run from bundled assets.
