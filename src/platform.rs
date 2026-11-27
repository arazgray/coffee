use std::{
    env,
    ffi::{OsStr, OsString},
    process::Command,
};

const VIDEO_EXTENSIONS: &str = "mp4 m4v mkv webm mov qt avi mpeg mpg m1v m2v m2t m2ts mts ts ogv wmv asf flv f4v vob 3gp 3g2 mxf dv rm rmvb nut";
const AUDIO_EXTENSIONS: &str = "mp3 mp2 flac wav wave m4a m4b opus aac ogg oga wma aiff aif aifc alac ac3 eac3 dts ape mka adts wv tta caf au amr";

fn media_extensions() -> impl Iterator<Item = &'static str> {
    [
        VIDEO_EXTENSIONS,
        AUDIO_EXTENSIONS,
        crate::state::PHOTO_EXTENSIONS,
    ]
    .into_iter()
    .flat_map(str::split_whitespace)
}

#[cfg(target_os = "linux")]
pub fn media_filter() -> String {
    media_extensions()
        .map(|ext| format!("*.{ext}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "linux")]
pub fn audio_filter() -> String {
    AUDIO_EXTENSIONS
        .split_whitespace()
        .map(|ext| format!("*.{ext}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn pick(picker: crate::Picker, recent: &std::path::Path) -> Option<String> {
    use crate::Picker::*;
    let base = rfd::FileDialog::new()
        .set_title("Coffee")
        .set_directory(recent);
    match picker {
        Files | Add => {
            let extensions: Vec<_> = media_extensions().collect();
            base.add_filter("Media files", &extensions)
                .add_filter("All files", &["*"])
                .pick_files()
                .map(|items| {
                    items
                        .iter()
                        .map(|p| p.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        }
        SlideshowMusic => base
            .add_filter(
                "Audio files",
                &AUDIO_EXTENSIONS.split_whitespace().collect::<Vec<_>>(),
            )
            .add_filter("All files", &["*"])
            .pick_file()
            .map(|file| file.to_string_lossy().into_owned()),
        Subtitles => base
            .add_filter("Subtitle files", &["srt", "vtt", "ass", "ssa", "sub"])
            .pick_file()
            .map(|file| file.to_string_lossy().into_owned()),
        Save => base
            .set_file_name("Playlist.coffee.json")
            .add_filter("Coffee playlists", &["json"])
            .save_file()
            .map(|file| file.to_string_lossy().into_owned()),
        Load => base
            .add_filter("Playlists", &["json", "m3u", "m3u8"])
            .pick_file()
            .map(|file| file.to_string_lossy().into_owned()),
    }
}

pub fn libmpv_paths() -> Vec<OsString> {
    if let Some(path) = env::var_os("COFFEE_LIBMPV").or_else(|| env::var_os("KAHVE_LIBMPV")) {
        return vec![path];
    }
    #[cfg(target_os = "macos")]
    {
        let app = env::current_exe().ok().and_then(|path| {
            Some(
                path.parent()?
                    .parent()?
                    .join("Frameworks/libmpv.2.dylib")
                    .into_os_string(),
            )
        });
        app.into_iter()
            .chain([
                "/opt/homebrew/opt/mpv/lib/libmpv.2.dylib".into(),
                "/usr/local/opt/mpv/lib/libmpv.2.dylib".into(),
                "libmpv.2.dylib".into(),
            ])
            .collect()
    }
    #[cfg(windows)]
    {
        windows_provider_paths("libmpv-2.dll")
    }
    #[cfg(target_os = "linux")]
    {
        vec!["libmpv.so.2".into()]
    }
}

pub fn libheif_paths() -> Vec<OsString> {
    #[cfg(target_os = "linux")]
    return vec!["libheif.so.1".into()];
    #[cfg(target_os = "macos")]
    return vec![
        "/opt/homebrew/opt/libheif/lib/libheif.1.dylib".into(),
        "/usr/local/opt/libheif/lib/libheif.1.dylib".into(),
        "libheif.1.dylib".into(),
    ];
    #[cfg(windows)]
    return windows_provider_paths("libheif.dll");
}

#[cfg(windows)]
fn windows_provider_paths(name: &str) -> Vec<OsString> {
    let root = env::var_os("MSYS2_ROOT").unwrap_or_else(|| r"C:\msys64".into());
    let sys = std::path::PathBuf::from(root)
        .join(r"ucrt64\bin")
        .join(name);
    env::current_exe()
        .ok()
        .and_then(|path| Some(path.parent()?.join(name).into_os_string()))
        .into_iter()
        .chain([sys.into_os_string()])
        .collect()
}

#[cfg(windows)]
pub fn dark_windows_frame(id: i64) {
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut std::ffi::c_void,
            attribute: u32,
            data: *const std::ffi::c_void,
            bytes: u32,
        ) -> i32;
    }
    let enabled = 1i32;
    // libmpv reports the HWND for the window it owns. Windows 11 and recent
    // Windows 10 builds accept DWMWA_USE_IMMERSIVE_DARK_MODE (20).
    unsafe {
        DwmSetWindowAttribute(
            id as usize as *mut _,
            20,
            std::ptr::from_ref(&enabled).cast(),
            4,
        );
    }
}

pub fn open_target(target: &OsStr) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(windows)]
    let mut cmd = if crate::state::is_stream(&target.to_string_lossy()) {
        let mut cmd = Command::new("rundll32.exe");
        cmd.arg("url.dll,FileProtocolHandler");
        cmd
    } else {
        Command::new("explorer.exe")
    };
    let mut child = cmd.arg(target).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn error_dialog(message: &str) {
    #[cfg(target_os = "linux")]
    if env::var_os("DISPLAY").is_some() || env::var_os("WAYLAND_DISPLAY").is_some() {
        let _ = crate::dialog().args(["--error", message]).status();
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        rfd::MessageDialog::new()
            .set_title("Coffee")
            .set_description(message)
            .show();
    }
}

const DEFAULT_WINDOW_SIZE: (i32, i32) = (1050, 630);
#[cfg(target_os = "macos")]
static MAC_WINDOW_SIZE: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();

pub fn initial_window_geometry() -> String {
    #[cfg(target_os = "macos")]
    let (w, h) = MAC_WINDOW_SIZE
        .get()
        .copied()
        .unwrap_or(DEFAULT_WINDOW_SIZE);
    #[cfg(not(target_os = "macos"))]
    let (w, h) = DEFAULT_WINDOW_SIZE;
    format!("{w}x{h}")
}

#[cfg(any(target_os = "macos", test))]
fn mac_window_size((visible_w, visible_h): (f64, f64), backing_scale: f64) -> (i32, i32) {
    let (w, h) = DEFAULT_WINDOW_SIZE;
    let fit = (visible_w * 0.9 / f64::from(w))
        .min(visible_h * 0.9 / f64::from(h))
        .clamp(0.01, 1.0);
    let to_pixels = |points| (f64::from(points) * fit * backing_scale).round() as i32;
    (to_pixels(w), to_pixels(h))
}

#[cfg(target_os = "macos")]
pub fn cocoa_loop(run: fn()) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy,
        NSScreen,
    };
    let marker = MainThreadMarker::new().expect("Cocoa must run on the main thread");
    let app = NSApplication::sharedApplication(marker);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // Keep Cocoa's native frame and the player UI in dark mode.
    let dark = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua) };
    app.setAppearance(dark.as_deref());
    if let Some(screen) = NSScreen::mainScreen(marker) {
        let visible = screen.visibleFrame().size;
        // mpv interprets absolute geometry sizes as backing pixels on Cocoa.
        let _ = MAC_WINDOW_SIZE.set(mac_window_size(
            (visible.width, visible.height),
            screen.backingScaleFactor(),
        ));
    }
    std::thread::spawn(move || {
        run();
        std::process::exit(0);
    });
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_window_geometry_stays_the_same_size_in_points_and_fits_small_displays() {
        assert_eq!(mac_window_size((1440.0, 875.0), 1.0), (1050, 630));
        assert_eq!(mac_window_size((1440.0, 875.0), 2.0), (2100, 1260));
        assert_eq!(mac_window_size((900.0, 520.0), 2.0), (1560, 936));
    }
}

pub fn ensure_dark_frame() {
    #[cfg(target_os = "linux")]
    {
        if !env::var("XDG_CURRENT_DESKTOP").is_ok_and(|s| s.split(':').any(|part| part == "KDE"))
            || !std::path::Path::new("/usr/share/color-schemes/BreezeDark.colors").is_file()
        {
            return;
        }
        let read = |key: &str| {
            let result = Command::new("kreadconfig6")
                .args(["--file", "kwinrulesrc", "--group", "General", "--key", key])
                .output()
                .ok()?;
            result
                .status
                .success()
                .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
        };
        let Some(mut rules) = read("rules") else {
            return;
        };
        let group = "coffee-dark-frame";
        if rules.split(',').any(|id| id == group) {
            return;
        }
        // Preserve numbered rules from configurations predating the rules list.
        if rules.is_empty() {
            let count = read("count")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0)
                .min(10000);
            rules = (1..=count)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
        }
        let write = |group, key, value| {
            Command::new("kwriteconfig6")
                .args([
                    "--file",
                    "kwinrulesrc",
                    "--group",
                    group,
                    "--key",
                    key,
                    value,
                ])
                .status()
                .is_ok_and(|s| s.success())
        };
        for (key, value) in [
            ("Description", "Coffee dark window frame"),
            ("wmclass", "coffee"),
            ("wmclassmatch", "1"),
            ("decocolor", "BreezeDark"),
            ("decocolorrule", "2"),
        ] {
            if !write(group, key, value) {
                return;
            }
        }
        if !rules.is_empty() {
            rules.push(',');
        }
        rules.push_str(group);
        if write("General", "rules", &rules) {
            let _ = Command::new("qdbus6")
                .args(["org.kde.KWin", "/KWin", "org.kde.KWin.reconfigure"])
                .output();
        }
    }
}
