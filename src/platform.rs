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

// Cocoa's hardware codes are layout-independent only for navigation keys.
// Use the event's actual character for printable keys, including non-ASCII text.
#[cfg(any(target_os = "macos", test))]
fn mac_key_name(
    code: u16,
    chars: &str,
    shift: bool,
    control: bool,
    command: bool,
) -> Option<String> {
    let special = match code {
        36 => "ENTER",
        48 => "TAB",
        51 => "BS",
        53 => "ESC",
        76 => "KP_ENTER",
        115 => "HOME",
        117 => "DEL",
        119 => "END",
        123 => "LEFT",
        124 => "RIGHT",
        125 => "DOWN",
        126 => "UP",
        _ => "",
    };
    if control || command {
        // Leave other system and menu shortcuts to Cocoa.
        if shift
            || control && command
            || !matches!((control, chars), (_, "a" | "o" | "v") | (false, "q"))
        {
            return None;
        }
        let modifier = if command { "Meta" } else { "Ctrl" };
        return Some(format!("{modifier}+{chars}"));
    }
    if !special.is_empty() {
        return Some(if shift {
            format!("Shift+{special}")
        } else {
            special.into()
        });
    }
    let mut text = chars.chars();
    let ch = text.next()?;
    if text.next().is_some() || ch.is_control() || ('\u{f700}'..='\u{f8ff}').contains(&ch) {
        return None;
    }
    Some(if ch == ' ' {
        "SPACE".into()
    } else {
        ch.to_string()
    })
}

#[cfg(target_os = "macos")]
mod cocoa_launch {
    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    };
    use objc2_app_kit::{NSApplication, NSApplicationDelegate};
    use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSURL};
    use std::{cell::RefCell, sync::mpsc::Sender};

    type Start = Box<dyn FnOnce(Vec<String>) + Send>;

    pub(super) struct State {
        start: Option<Start>,
        startup_files: Vec<String>,
        input: Sender<crate::Input>,
    }

    define_class!(
        // NSObject has no subclassing requirements. Finder sends these
        // callbacks on the AppKit thread.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = RefCell<State>]
        pub(super) struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl NSApplicationDelegate for Delegate {
            #[unsafe(method(application:openURLs:))]
            fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
                let files = (0..urls.count()).filter_map(|index| {
                    let url = urls.objectAtIndex(index);
                    if url.isFileURL() {
                        url.path()
                    } else {
                        url.absoluteString()
                    }
                    .map(|path| path.to_string())
                });
                let mut state = self.ivars().borrow_mut();
                if state.start.is_some() {
                    state.startup_files.extend(files);
                } else {
                    let _ = state.input.send(crate::Input::Launch(files.collect()));
                }
            }

            #[unsafe(method(applicationDidFinishLaunching:))]
            fn did_finish_launching(&self, _notification: &NSNotification) {
                let mut state = self.ivars().borrow_mut();
                if let Some(run) = state.start.take() {
                    let files = std::mem::take(&mut state.startup_files);
                    std::thread::spawn(move || {
                        run(files);
                        std::process::exit(0);
                    });
                }
            }
        }
    );

    impl Delegate {
        pub(super) fn new(
            marker: MainThreadMarker,
            start: impl FnOnce(Vec<String>) + Send + 'static,
            input: Sender<crate::Input>,
        ) -> Retained<Self> {
            let delegate = Self::alloc(marker).set_ivars(RefCell::new(State {
                start: Some(Box::new(start)),
                startup_files: Vec::new(),
                input,
            }));
            unsafe { msg_send![super(delegate), init] }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn cocoa_loop(
    run: impl FnOnce(Vec<String>) + Send + 'static,
    input: std::sync::mpsc::Sender<crate::Input>,
) {
    use objc2::{MainThreadMarker, runtime::ProtocolObject};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy,
        NSEvent, NSEventMask, NSEventModifierFlags as Modifiers, NSScreen,
    };
    use std::ptr::{self, NonNull};

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
    let delegate = cocoa_launch::Delegate::new(marker, run, input.clone());
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // mpv's executable forwards key events in its own NSApplication subclass.
    // libmpv uses our AppKit loop, so hand player keys to the Rust worker.
    let keyboard = block2::RcBlock::new(move |event: NonNull<NSEvent>| {
        let Some(marker) = MainThreadMarker::new() else {
            return event.as_ptr();
        };
        let app = NSApplication::sharedApplication(marker);
        let key_event = unsafe { event.as_ref() };
        if !app.isActive() || app.modalWindow().is_some() {
            return event.as_ptr();
        }
        let (Some(window), Some(key_window)) = (key_event.window(marker), app.keyWindow()) else {
            return event.as_ptr();
        };
        if !ptr::eq(&*window, &*key_window) || window.sheetParent().is_some() {
            return event.as_ptr();
        }
        let flags = key_event.modifierFlags();
        let control = flags.contains(Modifiers::Control);
        let command = flags.contains(Modifiers::Command);
        let chars = if command || control {
            key_event.charactersIgnoringModifiers()
        } else {
            key_event.characters()
        }
        .map(|text| text.to_string())
        .unwrap_or_default();
        let name = mac_key_name(
            key_event.keyCode(),
            &chars,
            flags.contains(Modifiers::Shift),
            control,
            command,
        );
        if let Some(name) = name
            && input
                .send(crate::Input::MacKey(name, key_event.isARepeat()))
                .is_ok()
        {
            return ptr::null_mut();
        }
        event.as_ptr()
    });
    // A local monitor runs on the AppKit thread and returns the original
    // borrowed event or nil; no event reference escapes the callback.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &keyboard)
    };
    app.run();
    app.setDelegate(None);
    if let Some(monitor) = monitor {
        unsafe { NSEvent::removeMonitor(&monitor) };
    }
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

    #[test]
    fn cocoa_key_names_preserve_text_and_reserve_system_combinations() {
        let key = |code, text, shift, ctrl, cmd| mac_key_name(code, text, shift, ctrl, cmd);
        assert_eq!(key(49, " ", false, false, false).as_deref(), Some("SPACE"));
        assert_eq!(
            key(124, "\u{f703}", false, false, false).as_deref(),
            Some("RIGHT")
        );
        assert_eq!(
            key(76, "\n", false, false, false).as_deref(),
            Some("KP_ENTER")
        );
        assert_eq!(key(44, "?", true, false, false).as_deref(), Some("?"));
        assert_eq!(key(31, "ö", false, false, false).as_deref(), Some("ö"));
        assert_eq!(key(31, "o", false, false, true).as_deref(), Some("Meta+o"));
        assert_eq!(key(9, "v", false, true, false).as_deref(), Some("Ctrl+v"));
        assert_eq!(key(46, "m", false, false, true), None);
        assert_eq!(key(122, "\u{f704}", false, false, false), None);
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
