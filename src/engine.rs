//! Minimal, dynamically linked libmpv client API. The mpv video output owns
//! the native Wayland surface; pixels never enter an SDR Rust GUI framebuffer.
use libloading::Library;
use serde_json::{Map, Value};
use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, CString, c_char, c_int, c_void},
    os::raw::c_double,
    path::{Path, PathBuf},
    ptr, slice,
};

type Handle = *mut c_void;
const MPV_FORMAT_NODE: c_int = 6;
const PLAYLIST_OBSERVER: u64 = 1;

#[repr(C)]
union NodeData {
    string: *mut c_char,
    flag: c_int,
    int64: i64,
    double_: c_double,
    list: *mut NodeList,
    bytes: *mut ByteArray,
}

#[repr(C)]
struct ByteArray {
    data: *const u8,
    size: usize,
}

#[repr(C)]
struct Node {
    data: NodeData,
    format: c_int,
}

#[repr(C)]
struct NodeList {
    num: c_int,
    values: *mut Node,
    keys: *mut *mut c_char,
}

#[repr(C)]
struct MpvEvent {
    id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct ClientMessage {
    num_args: c_int,
    args: *const *const c_char,
}

#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: i64,
}

#[repr(C)]
struct Hook {
    _name: *const c_char,
    id: u64,
}

pub enum Event {
    Shutdown,
    Start(i64),
    Loaded,
    FrameReady,
    End {
        id: i64,
        eof: bool,
        error: Option<String>,
    },
    Action(String),
    Pointer(PointerEvent),
    PlaylistChanged,
    TextInput(String),
    Changed,
}

pub enum PointerEvent {
    Down,
    Up,
    Click,
    Cancel,
}

struct Api {
    _lib: Library,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    destroy: unsafe extern "C" fn(Handle),
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(Handle, *const *const c_char) -> c_int,
    command_ret: unsafe extern "C" fn(Handle, *const *const c_char, *mut Node) -> c_int,
    get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    observe_property: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    free_node_contents: unsafe extern "C" fn(*mut Node),
    wait_event: unsafe extern "C" fn(Handle, c_double) -> *mut MpvEvent,
    hook_add: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    hook_continue: unsafe extern "C" fn(Handle, u64) -> c_int,
    stream_add:
        unsafe extern "C" fn(Handle, *const c_char, *mut c_void, crate::preload::OpenFn) -> c_int,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
}

impl Api {
    fn load() -> Result<Self, String> {
        let mut error = String::new();
        let lib = crate::platform::libmpv_paths()
            .into_iter()
            .find_map(|path| match load_provider(&path) {
                Ok(lib) => Some(lib),
                Err(err) => {
                    error = err.to_string();
                    None
                }
            })
            .ok_or_else(|| format!("Cannot load libmpv 0.41 or newer: {error}"))?;
        unsafe {
            macro_rules! symbol {
                ($symbol:literal) => {
                    *lib.get(concat!($symbol, "\0").as_bytes())
                        .map_err(|e| format!("{}: {e}", $symbol))?
                };
            }
            Ok(Self {
                create: symbol!("mpv_create"),
                initialize: symbol!("mpv_initialize"),
                destroy: symbol!("mpv_terminate_destroy"),
                set_option_string: symbol!("mpv_set_option_string"),
                command: symbol!("mpv_command"),
                command_ret: symbol!("mpv_command_ret"),
                get_property: symbol!("mpv_get_property"),
                observe_property: symbol!("mpv_observe_property"),
                free_node_contents: symbol!("mpv_free_node_contents"),
                wait_event: symbol!("mpv_wait_event"),
                hook_add: symbol!("mpv_hook_add"),
                hook_continue: symbol!("mpv_hook_continue"),
                stream_add: symbol!("mpv_stream_cb_add_ro"),
                error_string: symbol!("mpv_error_string"),
                _lib: lib,
            })
        }
    }

    fn error(&self, code: c_int) -> String {
        c_string(unsafe { (self.error_string)(code) })
    }
}

pub(crate) fn load_provider(path: &std::ffi::OsStr) -> Result<Library, libloading::Error> {
    #[cfg(windows)]
    if Path::new(path).is_absolute() {
        // Search the dependency's install directory and trusted loader
        // directories, not the process's current working directory.
        const SEARCH_DLL_DIR: u32 = 0x100;
        const SEARCH_DEFAULT_DIRS: u32 = 0x1000;
        return unsafe {
            libloading::os::windows::Library::load_with_flags(
                path,
                SEARCH_DLL_DIR | SEARCH_DEFAULT_DIRS,
            )
        }
        .map(Into::into);
    }
    unsafe { Library::new(path) }
}

fn c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // Each string comes from the live libmpv client or event.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn to_value(node: &Node) -> Value {
    // The active union member and buffer lengths are defined by libmpv's
    // node format. The caller has not freed the owning node yet.
    unsafe {
        match node.format {
            1 | 2 => Value::String(c_string(node.data.string)),
            3 => Value::Bool(node.data.flag != 0),
            4 => Value::from(node.data.int64),
            5 => Value::from(node.data.double_),
            7 | 8 if !node.data.list.is_null() => {
                let list = &*node.data.list;
                if list.num <= 0 || list.values.is_null() {
                    return if node.format == 7 {
                        Value::Array(vec![])
                    } else {
                        Value::Object(Map::new())
                    };
                }
                let values = slice::from_raw_parts(list.values, list.num as usize);
                if node.format == 7 {
                    Value::Array(values.iter().map(|item| to_value(item)).collect())
                } else if !list.keys.is_null() {
                    let keys = slice::from_raw_parts(list.keys, values.len());
                    Value::Object(
                        keys.iter()
                            .zip(values)
                            .map(|(&key, value)| (c_string(key), to_value(value)))
                            .collect(),
                    )
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        }
    }
}

pub struct Engine {
    api: Api,
    handle: Handle,
    image_source: RefCell<Option<tempfile::NamedTempFile>>,
    cache: Option<crate::preload::Cache>,
    stream_slot: Box<crate::preload::StreamSlot>,
    photo_zoom: Cell<crate::state::PhotoZoom>,
}

enum Output {
    Window,
    Test,
    Soundtrack,
}

pub struct Snapshot {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

const SNAPSHOT_ARGS: [*const c_char; 4] = [
    c"screenshot-raw".as_ptr(),
    c"scaled".as_ptr(),
    c"bgra".as_ptr(),
    ptr::null(),
];

impl Engine {
    pub fn create(
        input_conf: Option<&Path>,
        headless: bool,
        vulkan_device: Option<&str>,
    ) -> Result<Self, String> {
        Self::create_with_output(
            input_conf,
            if headless {
                Output::Test
            } else {
                Output::Window
            },
            vulkan_device,
        )
    }

    pub fn soundtrack(path: &str, volume: f64, muted: bool) -> Result<Self, String> {
        let mpv = Self::create_with_output(None, Output::Soundtrack, None)?;
        mpv.set("volume", &volume.to_string())?;
        mpv.set("mute", if muted { "yes" } else { "no" })?;
        mpv.command(&["loadfile", path])?;
        Ok(mpv)
    }

    fn create_with_output(
        input_conf: Option<&Path>,
        output: Output,
        vulkan_device: Option<&str>,
    ) -> Result<Self, String> {
        let api = Api::load()?;
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("libmpv could not create a client".into());
        }
        let mpv = Self {
            api,
            handle,
            image_source: RefCell::new(None),
            cache: (!matches!(output, Output::Soundtrack)).then(crate::preload::Cache::new),
            stream_slot: Box::default(),
            photo_zoom: Cell::default(),
        };
        let options = [
            ("config", "no"),
            ("terminal", "yes"),
            ("idle", "yes"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "yes"),
            ("load-scripts", "no"),
            ("load-context-menu", "no"),
            ("ytdl", "no"),
            ("keep-open", "yes"),
            ("image-display-duration", "inf"),
            ("audio-display", "embedded-first"),
            ("cache", "yes"),
            ("cache-secs", "45"),
            ("demuxer-max-bytes", "256MiB"),
            ("demuxer-max-back-bytes", "48MiB"),
            ("cache-pause-wait", "2"),
            ("volume-max", "100"),
            ("title", "Coffee"),
            ("audio-client-name", "Coffee"),
        ];
        for (key, value) in options {
            mpv.option(key, value)?;
        }
        if matches!(output, Output::Test) {
            for (key, value) in [("vo", "null"), ("ao", "null"), ("pause", "yes")] {
                mpv.option(key, value)?;
            }
        } else if matches!(output, Output::Soundtrack) {
            for (key, value) in [
                ("vo", "null"),
                ("vid", "no"),
                ("keep-open", "no"),
                ("loop-file", "inf"),
                ("terminal", "no"),
                ("audio-client-name", "Coffee Slideshow"),
            ] {
                mpv.option(key, value)?;
            }
        } else {
            let wayland =
                std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
            let geometry = crate::platform::initial_window_geometry();
            for (key, value) in [
                ("vo", "gpu-next"),
                ("force-window", "immediate"),
                ("hwdec", "auto-safe"),
                ("target-colorspace-hint", "auto"),
                ("target-colorspace-hint-mode", "target"),
                ("geometry", &geometry),
                ("auto-window-resize", "no"),
                ("keepaspect-window", "no"),
                // The compositor provides resize cursors for a decorated
                // Wayland window. X11 can keep the border without a title bar.
                ("border", "yes"),
                (
                    "title-bar",
                    if cfg!(any(windows, target_os = "macos")) {
                        "yes"
                    } else {
                        "no"
                    },
                ),
                ("window-dragging", "yes"),
                ("input-builtin-dragging", "no"),
                ("input-doubleclick-time", "0"),
                ("cursor-autohide", "3000"),
                ("drag-and-drop", "append"),
            ] {
                mpv.option(key, value)?;
            }
            if wayland {
                mpv.option("wayland-app-id", "coffee")?;
                mpv.option("wayland-edge-pixels-pointer", "12")?;
                // Prefer the display's GPU on hybrid laptops, keeping decoded
                // frames on the device where the compositor will use them.
                if let Some(device) = vulkan_device {
                    mpv.option("vulkan-device", device)?;
                }
                let context = std::env::var("KAHVE_GPU_CONTEXT")
                    .unwrap_or_else(|_| "waylandvk,wayland".to_owned());
                mpv.option("gpu-context", &context)?;
            }
            #[cfg(target_os = "linux")]
            mpv.option("x11-name", "coffee")?;
            if let Ok(decoder) = std::env::var("KAHVE_HWDEC") {
                mpv.option("hwdec", &decoder)?;
            }
        }
        if let Some(path) = input_conf {
            mpv.option("input-conf", &path.to_string_lossy())?;
        }
        if let Ok(level) = std::env::var("KAHVE_MPV_LOG") {
            mpv.option("msg-level", &level)?;
        }
        let code = unsafe { (mpv.api.initialize)(mpv.handle) };
        if code < 0 {
            return Err(mpv.api.error(code));
        }
        if matches!(output, Output::Soundtrack) {
            return Ok(mpv);
        }
        let stream = unsafe {
            (mpv.api.stream_add)(
                mpv.handle,
                c"coffee-ram".as_ptr(),
                ptr::from_ref(&*mpv.stream_slot).cast_mut().cast(),
                crate::preload::open,
            )
        };
        if stream < 0 {
            return Err(mpv.api.error(stream));
        }
        let hook = unsafe { (mpv.api.hook_add)(mpv.handle, 0, c"on_load".as_ptr(), 50) };
        if hook < 0 {
            return Err(mpv.api.error(hook));
        }
        Ok(mpv)
    }

    fn option(&self, key: &str, value: &str) -> Result<(), String> {
        let name = CString::new(key).map_err(|_| "Invalid option name")?;
        let val = CString::new(value).map_err(|_| "Invalid option value")?;
        let code =
            unsafe { (self.api.set_option_string)(self.handle, name.as_ptr(), val.as_ptr()) };
        if code < 0 {
            Err(format!("{key}: {}", self.api.error(code)))
        } else {
            Ok(())
        }
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let strings = args
            .iter()
            .map(|x| CString::new(*x))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Invalid command argument")?;
        let mut argv: Vec<_> = strings.iter().map(|s| s.as_ptr()).collect();
        argv.push(ptr::null());
        let code = unsafe { (self.api.command)(self.handle, argv.as_ptr()) };
        if code < 0 {
            Err(self.api.error(code))
        } else {
            Ok(())
        }
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.command(&["set", key, value])
    }

    pub fn preload(&self, files: Vec<PathBuf>) {
        if let Some(cache) = &self.cache {
            cache.plan(files);
        }
    }

    pub fn is_preloaded(&self, path: &Path) -> bool {
        self.cache
            .as_ref()
            .is_some_and(|cache| cache.get(path).is_some())
    }

    pub fn photo_zoom(&self, zoom: crate::state::PhotoZoom, apply_now: bool) -> Result<(), String> {
        self.photo_zoom.set(zoom);
        if apply_now {
            self.apply_photo_zoom()
        } else {
            Ok(())
        }
    }

    fn apply_photo_zoom(&self) -> Result<(), String> {
        for (key, value) in self.photo_zoom.get().options() {
            self.set(&format!("file-local-options/{key}"), value)?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        let dimensions = self.get("osd-dimensions");
        let pixels = dimensions["w"]
            .as_u64()?
            .checked_mul(dimensions["h"].as_u64()?)?;
        if pixels == 0 || pixels > 16 * 1024 * 1024 {
            return None;
        }
        // Capture the complete presentation area (including letterboxing)
        // without the UI. The new frame is still rendered by the same VO.
        let mut out = Node {
            data: NodeData { int64: 0 },
            format: 0,
        };
        if unsafe { (self.api.command_ret)(self.handle, SNAPSHOT_ARGS.as_ptr(), &mut out) } < 0 {
            return None;
        }
        let snapshot = unsafe { copy_snapshot(&out) };
        unsafe { (self.api.free_node_contents)(&mut out) };
        snapshot
    }

    pub fn crossfade_bitmap(&self, shot: &Snapshot, pixels: &[u8], display: (i32, i32)) -> bool {
        self.bitmap_overlay(
            "1",
            (shot.width, shot.height),
            pixels,
            (0, 0, display.0, display.1),
        )
    }

    pub fn bitmap_overlay(
        &self,
        id: &str,
        (width, height): (usize, usize),
        pixels: &[u8],
        (x, y, display_w, display_h): (i32, i32, i32, i32),
    ) -> bool {
        if width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(pixels.len()) {
            return false;
        }
        // overlay-add copies the buffer before the command returns.
        self.command(&[
            "overlay-add",
            id,
            &x.to_string(),
            &y.to_string(),
            &format!("&{}", pixels.as_ptr() as usize),
            "0",
            "bgra",
            &width.to_string(),
            &height.to_string(),
            &(width * 4).to_string(),
            &display_w.to_string(),
            &display_h.to_string(),
        ])
        .is_ok()
    }

    pub fn watch_playlist(&self) -> Result<(), String> {
        // MPV_FORMAT_NONE requests change notifications without copying data.
        let code = unsafe {
            (self.api.observe_property)(self.handle, PLAYLIST_OBSERVER, c"playlist".as_ptr(), 0)
        };
        if code < 0 {
            Err(self.api.error(code))
        } else {
            Ok(())
        }
    }

    pub fn get(&self, key: &str) -> Value {
        let Ok(key) = CString::new(key) else {
            return Value::Null;
        };
        let mut out = Node {
            data: NodeData { int64: 0 },
            format: 0,
        };
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                key.as_ptr(),
                MPV_FORMAT_NODE,
                ptr::from_mut(&mut out).cast(),
            )
        };
        if code < 0 {
            return Value::Null;
        }
        let copied = unsafe { to_value(&out) };
        unsafe { (self.api.free_node_contents)(&mut out) };
        copied
    }

    pub fn event(&self, seconds: f64) -> Option<Event> {
        let ptr = unsafe { (self.api.wait_event)(self.handle, seconds) };
        let event = unsafe { ptr.as_ref()? };
        Some(match event.id {
            0 => return None,
            1 => Event::Shutdown,
            6 => Event::Start(unsafe { *event.data.cast::<i64>().as_ref()? }),
            7 => {
                let end = unsafe { event.data.cast::<EndFile>().as_ref()? };
                Event::End {
                    id: end.playlist_entry_id,
                    eof: end.reason == 0,
                    error: (end.reason == 4).then(|| self.api.error(end.error)),
                }
            }
            8 => Event::Loaded,
            21 => Event::FrameReady,
            16 => {
                let message = unsafe { event.data.cast::<ClientMessage>().as_ref()? };
                if message.num_args < 2 || message.args.is_null() {
                    return Some(Event::Changed);
                }
                let args =
                    unsafe { slice::from_raw_parts(message.args, message.num_args as usize) };
                match c_string(args[0]).as_str() {
                    "kahve" => Event::Action(c_string(args[1])),
                    "key-binding" if args.len() >= 3 && c_string(args[1]) == "kahve-primary" => {
                        match c_string(args[2]).as_bytes() {
                            [_, _, b'c'] => Event::Pointer(PointerEvent::Cancel),
                            [b'd', ..] => Event::Pointer(PointerEvent::Down),
                            [b'u', ..] => Event::Pointer(PointerEvent::Up),
                            [b'p', ..] => Event::Pointer(PointerEvent::Click),
                            _ => Event::Changed,
                        }
                    }
                    "key-binding"
                        if args.len() >= 5 && c_string(args[1]) == "coffee-text-entry" =>
                    {
                        match c_string(args[2]).as_bytes().first() {
                            Some(b'd' | b'p' | b'r') => Event::TextInput(c_string(args[4])),
                            _ => Event::Changed,
                        }
                    }
                    _ => Event::Changed,
                }
            }
            22 if event.reply_userdata == PLAYLIST_OBSERVER => Event::PlaylistChanged,
            25 => {
                let id = unsafe { event.data.cast::<Hook>().as_ref()?.id };
                self.prepare_image_source();
                unsafe { (self.api.hook_continue)(self.handle, id) };
                Event::Changed
            }
            _ => Event::Changed,
        })
    }

    fn prepare_image_source(&self) {
        self.image_source.replace(None);
        self.stream_slot.clear();
        let source = self.get("stream-open-filename");
        let Some(path) = source.as_str() else { return };
        if crate::state::image_path(path) && self.photo_zoom.get() != crate::state::PhotoZoom::Fit {
            let _ = self.apply_photo_zoom();
        }
        let path = Path::new(path);
        if let Some(asset) = self.cache.as_ref().and_then(|cache| cache.get(path)) {
            let animated_webp = asset.animated_webp;
            let uri = self.stream_slot.stage(asset);
            if self.set("stream-open-filename", &uri).is_ok() {
                let _ = self.set("file-local-options/cache", "no");
                if animated_webp {
                    let _ = self.set("file-local-options/demuxer-lavf-o", "ignore_loop=0");
                }
                return;
            }
        }
        let media = crate::image_source::prepare_local(path);
        match media {
            Ok(Some(preview)) => {
                if self
                    .set("stream-open-filename", &preview.path().to_string_lossy())
                    .is_ok()
                {
                    if preview.path().extension().is_some_and(|ext| ext == "apng") {
                        let _ = self.set("file-local-options/demuxer-lavf-o", "ignore_loop=0");
                    }
                    self.image_source.replace(Some(preview));
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("{error}"),
        }
    }
}

unsafe fn copy_snapshot(node: &Node) -> Option<Snapshot> {
    if node.format != 8 {
        return None;
    }
    // The result and all of its children belong to libmpv until the caller
    // frees the node. Copy only validated rows using the reported stride.
    unsafe {
        let list = node.data.list.as_ref()?;
        if !(1..=16).contains(&list.num) || list.keys.is_null() || list.values.is_null() {
            return None;
        }
        let mut width = 0;
        let mut height = 0;
        let mut stride = 0;
        let mut data = None;
        for index in 0..list.num as usize {
            let key = CStr::from_ptr(*list.keys.add(index)).to_bytes();
            let value = &*list.values.add(index);
            match (key, value.format) {
                (b"w", 4) => width = usize::try_from(value.data.int64).ok()?,
                (b"h", 4) => height = usize::try_from(value.data.int64).ok()?,
                (b"stride", 4) => stride = usize::try_from(value.data.int64).ok()?,
                (b"data", 9) => data = value.data.bytes.as_ref(),
                _ => {}
            }
        }
        let data = data?;
        let row = width.checked_mul(4)?;
        let length = row.checked_mul(height)?;
        let required = stride
            .checked_mul(height.checked_sub(1)?)?
            .checked_add(row)?;
        if length == 0
            || length > 64 * 1024 * 1024
            || stride < row
            || required > data.size
            || data.data.is_null()
        {
            return None;
        }
        let src = slice::from_raw_parts(data.data, required);
        let mut bgra = Vec::with_capacity(length);
        for y in 0..height {
            bgra.extend_from_slice(&src[y * stride..y * stride + row]);
        }
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Some(Snapshot {
            width,
            height,
            bgra,
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.handle) };
    }
}
