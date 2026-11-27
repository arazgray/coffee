#![cfg_attr(windows, windows_subsystem = "windows")]

mod crossfade;
mod engine;
mod fonts;
mod gpu;
mod heif_source;
mod image_source;
mod library;
mod photos;
mod platform;
mod preload;
mod prepare;
mod scale;
mod seek_preview;
mod state;
mod subtitles;
mod ui;

use crossfade::Crossfade;
use engine::{Engine, Event, PointerEvent};
use gpu::{DecodeChoice, Devices};
use photos::{PanBounds, PhotoFolder};
use state::{
    Hdr, Kind, Media, PhotoZoom, RenderMode, SpeedStep, display_name, image_path, is_stream,
};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    fs::DirBuilder,
    io::{Read, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
};
use ui::{Action, Frame, Menu, View};

#[derive(Clone, Copy)]
enum Picker {
    Files,
    Add,
    SlideshowMusic,
    Subtitles,
    Save,
    Load,
}

enum Input {
    #[cfg(unix)]
    Launch(Vec<String>),
    Picked(Picker, Option<String>),
    FontsLoaded(fonts::Catalog),
}

struct Runtime {
    #[cfg(unix)]
    socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(self.dir.join("input.conf"));
        let _ = fs::remove_dir(&self.dir);
    }
}

#[cfg(unix)]
fn connect(socket: &Path, files: &[String]) -> bool {
    let Ok(mut client) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = client.set_write_timeout(Some(Duration::from_secs(2)));
    serde_json::to_writer(&mut client, files).is_ok() && client.flush().is_ok()
}

#[cfg(unix)]
fn runtime(files: &[String], tx: Sender<Input>) -> Result<Option<Runtime>, String> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or("Coffee needs a user runtime directory")?;
    let root = base.join("coffee");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(|e| e.to_string())?;
    let socket = root.join("player.sock");
    if connect(&socket, files) {
        return Ok(None);
    }
    let dir = root.join(format!("window-{}", std::process::id()));
    DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    let listener = match UnixListener::bind(&socket) {
        Ok(server) => server,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            if connect(&socket, files) {
                let _ = fs::remove_dir(&dir);
                return Ok(None);
            }
            // A crash may have left this app's socket in its private directory.
            use std::os::unix::fs::FileTypeExt;
            if !fs::symlink_metadata(&socket).is_ok_and(|meta| meta.file_type().is_socket()) {
                return Err("Coffee's runtime socket path is already in use".into());
            }
            fs::remove_file(&socket).map_err(|e| e.to_string())?;
            UnixListener::bind(&socket).map_err(|e| e.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let parsed = serde_json::from_reader::<_, Vec<String>>(stream.take(1024 * 1024));
            if let Ok(files) = parsed
                && tx.send(Input::Launch(files)).is_err()
            {
                break;
            }
        }
    });
    Ok(Some(Runtime { socket, dir }))
}

#[cfg(not(unix))]
fn runtime(_files: &[String], _tx: Sender<Input>) -> Result<Option<Runtime>, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = env::temp_dir().join(format!("coffee-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).map_err(|error| error.to_string())?;
    Ok(Some(Runtime { dir }))
}

#[derive(Default)]
struct Timings {
    paused: bool,
    muted: bool,
    repeat_one: bool,
    fullscreen: bool,
    maximized: bool,
    border: bool,
    buffering: bool,
    duration: f64,
    position: f64,
    buffered: f64,
    volume: f64,
    speed: f64,
    reverse: bool,
    seekable: bool,
    dimensions: (i32, i32),
    backing_scale: f64,
    pan_bounds: PanBounds,
    mouse: Option<(i32, i32)>,
}

fn mouse_position(mpv: &Engine) -> Option<(i32, i32)> {
    let mouse = mpv.get("mouse-pos");
    if mouse["hover"].as_bool() != Some(true) {
        return None;
    }
    Some((mouse["x"].as_f64()? as i32, mouse["y"].as_f64()? as i32))
}

impl Timings {
    fn read(mpv: &Engine) -> Self {
        let dimensions = mpv.get("osd-dimensions");
        let size = (
            dimensions["w"].as_i64().unwrap_or_default() as i32,
            dimensions["h"].as_i64().unwrap_or_default() as i32,
        );
        Self {
            paused: mpv.get("pause").as_bool().unwrap_or(true),
            muted: mpv.get("mute").as_bool().unwrap_or(false),
            repeat_one: mpv.get("loop-file").as_str() == Some("inf"),
            fullscreen: mpv.get("fullscreen").as_bool().unwrap_or(false),
            maximized: mpv.get("window-maximized").as_bool().unwrap_or(false),
            border: mpv.get("border").as_bool().unwrap_or(false),
            buffering: mpv.get("paused-for-cache").as_bool().unwrap_or(false),
            duration: mpv.get("duration").as_f64().unwrap_or_default(),
            position: mpv.get("time-pos").as_f64().unwrap_or_default(),
            buffered: mpv
                .get("demuxer-cache-duration")
                .as_f64()
                .unwrap_or_default(),
            volume: mpv.get("volume").as_f64().unwrap_or(100.0),
            speed: mpv.get("speed").as_f64().unwrap_or(1.0),
            reverse: mpv.get("play-direction").as_str() == Some("backward"),
            seekable: mpv.get("seekable").as_bool() == Some(true),
            dimensions: size,
            // Cocoa reports the video surface and pointer in backing pixels.
            #[cfg(target_os = "macos")]
            backing_scale: mpv
                .get("display-hidpi-scale")
                .as_f64()
                .filter(|scale| scale.is_finite() && *scale > 0.0)
                .unwrap_or(1.0),
            #[cfg(not(target_os = "macos"))]
            backing_scale: 1.0,
            pan_bounds: PanBounds::from_margins(
                size,
                ["ml", "mr", "mt", "mb"].map(|k| dimensions[k].as_i64().unwrap_or_default()),
            ),
            mouse: mouse_position(mpv),
        }
    }
}

struct LivePreview {
    photo_id: i64,
    clip_id: i64,
}

const SLIDE_INTERVAL: Duration = Duration::from_secs(5);
const SLIDE_CONTROLS_TIMEOUT: Duration = Duration::from_secs(2);

struct Slideshow {
    next: Instant,
    music: Option<Engine>,
    reveal_after: Instant,
    controls_until: Option<Instant>,
}

struct PhotoDrag {
    origin: (i32, i32),
    position: [f64; 2],
    bounds: PanBounds,
    moved: bool,
}

struct StreamInput {
    text: String,
    cursor: usize,
    all: bool,
    invalid: bool,
}

impl StreamInput {
    fn open() -> Self {
        Self {
            text: "https://".into(),
            cursor: 8,
            all: false,
            invalid: false,
        }
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }

    fn clear_selection(&mut self) -> bool {
        if !self.all {
            return false;
        }
        self.text.clear();
        self.cursor = 0;
        self.all = false;
        true
    }

    fn insert(&mut self, text: &str) {
        let input: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(4096)
            .collect();
        if input.is_empty() {
            return;
        }
        self.clear_selection();
        if self.text.len() + input.len() > 8192 {
            return;
        }
        self.text.insert_str(self.cursor, &input);
        self.cursor += input.len();
        self.invalid = false;
    }
}

#[derive(Default)]
struct QueueSnapshot {
    ids: Vec<i64>,
    current: Option<i64>,
}

impl QueueSnapshot {
    fn read(mpv: &Engine) -> Self {
        let mut snapshot = Self::default();
        let value = mpv.get("playlist");
        for entry in value.as_array().into_iter().flatten() {
            let Some(id) = entry["id"].as_i64() else {
                continue;
            };
            snapshot.ids.push(id);
            if entry["current"].as_bool() == Some(true) {
                snapshot.current = Some(id);
            }
        }
        snapshot
    }

    fn first_drop(&self, new: &Self) -> Option<usize> {
        (self.current.is_some()
            && self.current == new.current
            && new.ids.len() > self.ids.len()
            && new.ids.starts_with(&self.ids))
        .then_some(self.ids.len())
    }
}

const DROP_BATCH: Duration = Duration::from_millis(40);

const DOUBLE_CLICK: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClickArea {
    Video,
    TopBar,
    BottomBar,
}

struct PendingClick {
    area: ClickArea,
    point: (i32, i32),
    at: Instant,
}

impl PendingClick {
    fn matches(&self, area: ClickArea, point: (i32, i32), now: Instant) -> bool {
        self.area == area
            && now.duration_since(self.at) < DOUBLE_CLICK
            && self
                .point
                .0
                .abs_diff(point.0)
                .max(self.point.1.abs_diff(point.1))
                <= 8
    }
}

const CONTROLS_FADE: Duration = Duration::from_millis(200);

struct ControlsFade {
    from: f64,
    visible: bool,
    started: Instant,
}

impl ControlsFade {
    fn new(now: Instant) -> Self {
        Self {
            from: 1.0,
            visible: true,
            started: now,
        }
    }
    fn active(&self, now: Instant) -> bool {
        self.from != self.target() && now.duration_since(self.started) < CONTROLS_FADE
    }
    fn hide_now(&mut self, now: Instant) {
        self.from = 0.0;
        self.visible = false;
        self.started = now;
    }
    fn target(&self) -> f64 {
        if self.visible { 1.0 } else { 0.0 }
    }
    fn opacity(&self, now: Instant) -> f64 {
        let p =
            (now.duration_since(self.started).as_secs_f64() / CONTROLS_FADE.as_secs_f64()).min(1.0);
        self.from + (self.target() - self.from) * p
    }
    fn update(&mut self, visible: bool, now: Instant) -> u8 {
        if self.visible != visible {
            self.from = self.opacity(now);
            self.visible = visible;
            self.started = now;
        }
        (self.opacity(now) * 255.0).round() as u8
    }
}

struct App {
    mpv: Engine,
    tx: Sender<Input>,
    rx: Receiver<Input>,
    media: Media,
    timing: Timings,
    frame: Option<Frame>,
    menu: Option<Menu>,
    context_anchor: Option<(i32, i32)>,
    scroll: usize,
    hdr: Hdr,
    devices: Devices,
    decoder: DecodeChoice,
    render_mode: RenderMode,
    fast_render: bool,
    quality: &'static str,
    toast: (String, Instant),
    hovered: Instant,
    controls: ControlsFade,
    pressed: Option<Action>,
    drag_origin: Option<(i32, i32)>,
    photo_drag: Option<PhotoDrag>,
    photo_pan: [f64; 2],
    pending_click: Option<PendingClick>,
    last_volume: f64,
    picker: bool,
    recent: PathBuf,
    fade: Option<Crossfade>,
    active_file: Option<i64>,
    photo_switch: Option<(i64, usize)>,
    zoom: PhotoZoom,
    slideshow: Option<Slideshow>,
    slideshow_music: Option<String>,
    photos: Option<PhotoFolder>,
    live: Option<LivePreview>,
    stream: Option<StreamInput>,
    subtitles: subtitles::Subtitles,
    fonts: fonts::Catalog,
    font_query: String,
    font_input: bool,
    prepare_target: Option<prepare::Target>,
    prepare_job: Option<prepare::Job>,
    prepared_track: Option<(i64, i64)>,
    playback_health: prepare::PlaybackHealth,
    reverse_hwdec: Option<String>,
    seek_preview: seek_preview::Preview,
    queue_snapshot: QueueSnapshot,
    playlist_changed: Option<Instant>,
}

impl App {
    fn stream_dialog(&mut self, cancel: bool) -> Result<(), String> {
        if cancel {
            self.stream = None;
            self.mpv.command(&["disable-section", "coffee-stream"])
        } else {
            self.mpv.command(&[
                "define-section",
                "coffee-stream",
                include_str!("../assets/stream-input.conf"),
                "force",
            ])?;
            self.mpv
                .command(&["enable-section", "coffee-stream", "exclusive"])?;
            self.stream = Some(StreamInput::open());
            self.pending_click = None;
            Ok(())
        }
    }

    fn submit_stream(&mut self) -> Result<(), String> {
        let Some(stream) = &mut self.stream else {
            return Ok(());
        };
        let url = stream.text.trim();
        if !is_stream(url) || library::normalize(url).is_none() {
            stream.invalid = true;
            return Ok(());
        }
        let url = url.to_owned();
        self.stream_dialog(true)?;
        self.queue(&[url], false, 0);
        Ok(())
    }

    fn stream_key(&mut self, key: &str) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        let result = match key {
            "stream-submit" => self.submit_stream(),
            "stream-cancel" => {
                self.stop_slideshow();
                self.stream_dialog(true)
            }
            "stream-paste" => {
                let paste = self.mpv.get("clipboard/text");
                if let Some(text) = paste.as_str() {
                    let text = text.trim();
                    if stream.text == "https://" && is_stream(text) {
                        stream.all = true;
                    }
                    stream.insert(text);
                }
                Ok(())
            }
            "stream-all" => {
                stream.all = true;
                Ok(())
            }
            "stream-backspace" | "stream-delete" => {
                if !stream.clear_selection() {
                    let (start, end) = if key == "stream-backspace" {
                        (stream.previous(), stream.cursor)
                    } else {
                        (stream.cursor, stream.next())
                    };
                    stream.text.replace_range(start..end, "");
                    stream.cursor = start;
                }
                stream.invalid = false;
                Ok(())
            }
            "stream-left" | "stream-right" | "stream-home" | "stream-end" => {
                stream.cursor = match key {
                    "stream-home" => 0,
                    "stream-end" => stream.text.len(),
                    "stream-left" => stream.previous(),
                    _ => stream.next(),
                };
                stream.all = false;
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.note(error);
        }
    }

    fn refresh_media(&mut self) {
        if self.photo_switch.is_some() {
            // Keep the outgoing photo HUD until mpv has loaded the new
            // item. Several properties can become empty in between.
            return;
        }
        self.media = Media::read(&self.mpv);
        if self.media.kind == Kind::Video {
            self.subtitles.refresh(&self.mpv);
        }
        if let Err(error) = self.update_render_profile() {
            self.note(error);
        }
    }

    fn update_render_profile(&mut self) -> Result<(), String> {
        let fast = self.render_mode.fast(self.media.high_rate_4k);
        if fast != self.fast_render {
            RenderMode::apply(fast, &self.mpv)?;
            self.fast_render = fast;
        }
        Ok(())
    }

    fn queue(&mut self, paths: &[String], append: bool, current: usize) {
        let mut valid = paths.iter().filter_map(|p| library::normalize(p));
        if let Some(first) = valid.next() {
            if !append {
                self.stop_slideshow();
                self.photo_switch = None;
                self.photos = None;
                self.clear_crossfade();
                self.finish_live(false);
            }
            let _ = self.mpv.command(&[
                "loadfile",
                &first,
                if append { "append-play" } else { "replace" },
            ]);
            for path in valid {
                let _ = self.mpv.command(&["loadfile", &path, "append"]);
            }
            if !append && current != 0 {
                let _ = self
                    .mpv
                    .command(&["playlist-play-index", &current.to_string()]);
            }
            if !append {
                let _ = self.mpv.set("pause", "no");
            }
            // In-app opens, queue loads and Add files aren't native drops.
            self.queue_snapshot = QueueSnapshot::read(&self.mpv);
        }
    }

    fn prepare_photos(&mut self) {
        if self.media.kind != Kind::Image {
            self.photos = None;
            self.mpv.preload(Vec::new());
            return;
        }
        if self
            .photos
            .as_mut()
            .is_some_and(|folder| folder.select(Path::new(&self.media.path)))
        {
            self.warm_photos();
            return;
        }
        self.photos = PhotoFolder::scan(&self.media.path);
        self.warm_photos();
        if let Some(folder) = &self.photos
            && self.media.playlist.len() == 1
            && folder.files().len() > 1
        {
            // Eagerly queue the siblings, not their pixels. Keep the
            // currently decoded still and don't touch an explicit playlist.
            for (index, path) in folder.files().iter().enumerate() {
                if index != folder.index {
                    let _ = self
                        .mpv
                        .command(&["loadfile", &path.to_string_lossy(), "append"]);
                }
            }
            if folder.index != 0 {
                let _ = self
                    .mpv
                    .command(&["playlist-move", "0", &(folder.index + 1).to_string()]);
            }
            self.queue_snapshot = QueueSnapshot::read(&self.mpv);
            self.refresh_media();
        }
    }

    fn warm_photos(&self) {
        self.mpv.preload(
            self.photos
                .as_ref()
                .map(PhotoFolder::preload_paths)
                .unwrap_or_default(),
        );
    }

    fn play_dropped_files(&mut self) {
        if !self
            .playlist_changed
            .is_some_and(|at| at.elapsed() >= DROP_BATCH)
        {
            return;
        }
        self.playlist_changed = None;
        let before = std::mem::replace(&mut self.queue_snapshot, QueueSnapshot::read(&self.mpv));
        // libmpv appends all drop entries in order. It auto-starts an empty
        // playlist; otherwise select the first new entry without replacing
        // the existing queue. Batch the notifications for multi-file drops.
        if let Some(index) = before.first_drop(&self.queue_snapshot) {
            self.stop_slideshow();
            self.photo_switch = None;
            self.clear_crossfade();
            self.pending_click = None;
            if let Err(error) = self
                .mpv
                .command(&["playlist-play-index", &index.to_string()])
                .and_then(|()| self.mpv.set("pause", "no"))
            {
                self.note(error);
            }
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.toast = (text.into(), Instant::now());
        self.hovered = Instant::now();
    }

    fn panel(&mut self, menu: Menu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.scroll = 0;
        if self.menu == Some(Menu::Playlist) {
            self.scroll = self.media.index.unwrap_or(0).saturating_sub(3);
        }
        if self.menu == Some(Menu::SubtitleFont) {
            self.font_query.clear();
            if matches!(
                self.fonts,
                fonts::Catalog::Unloaded | fonts::Catalog::Failed
            ) {
                let tx = self.tx.clone();
                self.fonts = fonts::Catalog::Loading;
                thread::spawn(move || {
                    let _ = tx.send(Input::FontsLoaded(fonts::discover()));
                });
            }
        }
    }

    fn sync_font_input(&mut self) {
        let wanted = self.menu == Some(Menu::SubtitleFont) && self.stream.is_none();
        if wanted != self.font_input {
            self.font_input = wanted;
            let args: &[&str] = if wanted {
                &["enable-section", "coffee-subtitle-font", "exclusive"]
            } else {
                &["disable-section", "coffee-subtitle-font"]
            };
            let _ = self.mpv.command(args);
        }
    }

    fn type_font_query(&mut self, text: &str) {
        let room = 96usize.saturating_sub(self.font_query.chars().count());
        self.font_query
            .extend(text.chars().filter(|c| !c.is_control()).take(room));
        self.scroll = 0;
        self.hovered = Instant::now();
    }

    fn font_key(&mut self, key: &str) {
        if self.menu != Some(Menu::SubtitleFont) {
            return;
        }
        match key {
            "font-backspace" => {
                self.font_query.pop();
            }
            "font-clear" => self.font_query.clear(),
            "font-space" => self.type_font_query(" "),
            "font-paste" => {
                let paste = self.mpv.get("clipboard/text");
                if let Some(text) = paste.as_str() {
                    self.type_font_query(text);
                }
            }
            _ => return,
        }
        self.scroll = 0;
    }

    fn sample_playback(&mut self, now: Instant) {
        if self.prepare_target.is_none() || self.playback_health.detected() {
            return;
        }
        if self.timing.paused
            || self.timing.buffering
            || self.timing.reverse
            || (self.timing.speed - 1.0).abs() > 0.01
            || self.timing.position < 0.25
            || self.timing.duration - self.timing.position < 0.5
        {
            self.playback_health.interrupt();
            return;
        }
        if !self.playback_health.due(now) {
            return;
        }
        if self.mpv.get("seeking").as_bool() == Some(true) {
            self.playback_health.interrupt();
            return;
        }
        let count = |property| self.mpv.get(property).as_u64().unwrap_or_default();
        let drops = count("frame-drop-count").saturating_add(count("decoder-frame-drop-count"));
        let fps = self
            .mpv
            .get("container-fps")
            .as_f64()
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .unwrap_or(30.0);
        self.playback_health
            .observe(now, self.timing.position, self.timing.speed, fps, drops);
        if self.playback_health.detected() {
            // Reveal the offer even when playback controls were hidden.
            self.hovered = now;
        }
    }

    fn step_playback_speed(&mut self, step: SpeedStep) -> Result<(), String> {
        if !matches!(self.media.kind, Kind::Video | Kind::Music) || self.live.is_some() {
            return Ok(());
        }
        let was_reverse = self.mpv.get("play-direction").as_str() == Some("backward");
        let current = self.mpv.get("speed").as_f64().unwrap_or(1.0);
        let current = if was_reverse { -current } else { current };
        let Some(speed) = step.next(current, self.timing.seekable) else {
            return Ok(());
        };
        let reverse = speed < 0.0;
        self.playback_health.interrupt();
        if reverse && !was_reverse && self.media.kind == Kind::Video {
            // mpv has to retain whole decoded GOPs in reverse. Keeping those
            // frames in hardware surfaces can exhaust the GPU decoder pool.
            let old = match self.mpv.get("hwdec") {
                serde_json::Value::String(mode) => mode,
                serde_json::Value::Array(modes) if modes.is_empty() => "no".into(),
                serde_json::Value::Array(modes) => modes
                    .iter()
                    .filter_map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                _ => "auto-safe".into(),
            };
            self.mpv.set("file-local-options/hwdec", "no")?;
            self.reverse_hwdec = Some(old);
        }
        if reverse != was_reverse {
            self.mpv.set(
                "file-local-options/play-direction",
                if reverse { "backward" } else { "forward" },
            )?;
        }
        if !reverse && let Some(old) = &self.reverse_hwdec {
            self.mpv.set("file-local-options/hwdec", old)?;
            self.reverse_hwdec = None;
        }
        self.mpv
            .set("file-local-options/speed", &speed.abs().to_string())?;
        if reverse && !was_reverse && self.timing.position <= 0.1 && self.timing.duration > 0.25 {
            self.mpv.command(&[
                "seek",
                &(self.timing.duration - 0.1).to_string(),
                "absolute+exact",
            ])?;
        }
        Ok(())
    }

    fn preview_original(&mut self) {
        let source = (self.media.kind == Kind::Video && self.live.is_none())
            .then(|| library::normalize(&self.media.path).map(PathBuf::from))
            .flatten()
            .filter(|path| path.is_absolute() && path.is_file())
            .map(|path| (path, self.media.source_range.starts_with("HDR")));
        self.seek_preview.source(&self.mpv, source);
    }

    fn start_preparation(&mut self) -> Result<(), String> {
        if self.prepare_job.as_ref().is_some_and(|job| {
            matches!(
                job.status(),
                prepare::Status::Running(..) | prepare::Status::Ready
            )
        }) {
            return Ok(());
        }
        let target = self
            .prepare_target
            .clone()
            .ok_or("Open a local video first.")?;
        if target.cached {
            return Ok(());
        }
        let duration = self.mpv.get("duration").as_f64().unwrap_or_default();
        let was_paused = self.timing.paused;
        self.mpv.set("pause", "yes")?;
        match prepare::Job::start(target, duration, self.devices.preparation_device()) {
            Ok(job) => {
                self.prepare_job = Some(job);
                Ok(())
            }
            Err(error) => {
                if !was_paused {
                    let _ = self.mpv.set("pause", "no");
                }
                Err(error)
            }
        }
    }

    fn play_prepared(&mut self) -> Result<(), String> {
        if self.prepared_track.is_some() {
            return Ok(());
        }
        let target = self
            .prepare_target
            .as_ref()
            .ok_or("Open a local video first.")?;
        if !target.cached
            && !self
                .prepare_job
                .as_ref()
                .is_some_and(|job| matches!(job.status(), prepare::Status::Ready))
        {
            return Err("The prepared video isn’t ready yet.".into());
        }
        if !target.still_matches() {
            return Err("The prepared copy or original changed. Reopen the original video.".into());
        }
        let original = self
            .mpv
            .get("current-tracks/video/id")
            .as_i64()
            .ok_or("No active video track.")?;
        self.mpv.command(&[
            "video-add",
            &target.output.to_string_lossy(),
            "select",
            "Coffee · Prepared 1080p SDR",
        ])?;
        let prepared = self
            .mpv
            .get("current-tracks/video/id")
            .as_i64()
            .filter(|id| *id != original)
            .ok_or("Couldn’t select the prepared video.")?;
        self.prepared_track = Some((original, prepared));
        self.seek_preview
            .source(&self.mpv, Some((target.output.clone(), false)));
        self.menu = None;
        self.mpv.set("pause", "no")
    }

    fn play_original(&mut self) -> Result<(), String> {
        if let Some((original, prepared)) = self.prepared_track {
            self.mpv.set("vid", &original.to_string())?;
            self.prepared_track = None;
            self.mpv.command(&["video-remove", &prepared.to_string()])?;
            self.preview_original();
        }
        self.menu = None;
        Ok(())
    }

    fn next(&mut self, direction: i32) {
        // Use the already selected entry, even mid-load. Avoid a full
        // metadata refresh or GPU readback on the navigation hot path.
        let current = self.photo_switch.map(|(_, index)| index).or_else(|| {
            self.mpv
                .get("playlist-pos")
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
        });
        if self.media.kind == Kind::Image
            && let Some(folder) = &mut self.photos
            && let Some(path) = current.and_then(|index| self.media.playlist.get(index))
            && folder.select(Path::new(path))
        {
            let Some(path) = folder
                .adjacent(direction)
                .map(|p| p.to_string_lossy().into_owned())
            else {
                return;
            };
            if let Err(error) = self.select_photo(path) {
                self.note(error);
            }
            return;
        }
        let Some(index) = current else {
            return;
        };
        let next = (index as i64 + i64::from(direction))
            .try_into()
            .ok()
            .filter(|i: &usize| *i < self.media.playlist.len());
        if let Some(index) = next {
            if let Err(error) = self.play_index(index) {
                self.note(error);
            }
        } else if self.media.kind == Kind::Image {
            self.note(if direction < 0 {
                "First image in the folder."
            } else {
                "Last image in the folder."
            });
        }
    }

    fn select_photo(&mut self, path: String) -> Result<(), String> {
        let index = if let Some(index) = self.media.playlist.iter().position(|entry| entry == &path)
        {
            index
        } else {
            self.mpv.command(&["loadfile", &path, "append"])?;
            self.queue_snapshot = QueueSnapshot::read(&self.mpv);
            self.media.playlist.push(path);
            self.queue_snapshot
                .ids
                .len()
                .checked_sub(1)
                .ok_or("Could not queue photo")?
        };
        self.play_index(index)
    }

    fn start_slideshow(&mut self) -> Result<(), String> {
        if self.media.kind != Kind::Image
            || self.photos.is_none()
            || self.picker
            || self.slideshow.is_some()
        {
            return Ok(());
        }
        self.mpv.set("fullscreen", "yes")?;
        self.menu = None;
        let mut music = None;
        if let Some(path) = &self.slideshow_music {
            match Engine::soundtrack(path, self.timing.volume, self.timing.muted) {
                Ok(player) => music = Some(player),
                Err(error) => self.note(format!("Couldn’t start slideshow music: {error}")),
            }
        }
        let now = Instant::now();
        self.controls.hide_now(now);
        self.slideshow = Some(Slideshow {
            next: now + SLIDE_INTERVAL,
            music,
            // Fullscreen can move the reported pointer without user input.
            reveal_after: now + Duration::from_millis(400),
            controls_until: None,
        });
        Ok(())
    }

    fn stop_slideshow(&mut self) {
        if self.slideshow.take().is_some() {
            self.menu = None;
            let _ = self.mpv.set("fullscreen", "no");
        }
    }

    fn audio_controls(&self) -> &Engine {
        self.slideshow
            .as_ref()
            .and_then(|slideshow| slideshow.music.as_ref())
            .unwrap_or(&self.mpv)
    }

    fn tick_slideshow(&mut self) {
        let Some(slideshow) = &mut self.slideshow else {
            return;
        };
        if let Some(mpv) = &slideshow.music {
            let mut failed = None;
            for _ in 0..8 {
                match mpv.event(0.0) {
                    Some(Event::End {
                        error: Some(error), ..
                    }) => {
                        failed = Some(error);
                        break;
                    }
                    Some(Event::Shutdown) => {
                        failed = Some("audio player stopped".into());
                        break;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            if let Some(error) = failed {
                slideshow.music = None;
                self.note(format!("Couldn’t play slideshow music: {error}"));
                return;
            }
        }
        if self.photo_switch.is_some()
            || self.photo_drag.is_some()
            || self.live.is_some()
            || Instant::now() < slideshow.next
        {
            return;
        }
        slideshow.next = Instant::now() + SLIDE_INTERVAL;
        if let Some(path) = self.photos.as_ref().and_then(PhotoFolder::next_slide) {
            let path = path.to_string_lossy().into_owned();
            if let Err(error) = self.select_photo(path) {
                self.stop_slideshow();
                self.note(error);
            }
        }
    }

    fn live_photo(&mut self) -> Result<(), String> {
        if self.live.is_some() {
            self.finish_live(true);
            return Ok(());
        }
        let clip = self
            .photos
            .as_ref()
            .and_then(PhotoFolder::live_clip)
            .filter(|_| self.media.kind == Kind::Image && self.photo_switch.is_none())
            .ok_or("No Live Photo clip")?;
        let queue = QueueSnapshot::read(&self.mpv);
        let photo_id = queue.current.ok_or("No active photo")?;
        let index = queue.ids.len();
        // Retain the clip's final frame for the return blend. A Live Photo
        // preview plays once even when Repeat One is enabled in the player.
        self.mpv.command(&[
            "loadfile",
            &clip.to_string_lossy(),
            "append",
            "-1",
            "keep-open=always,loop-file=no",
        ])?;
        self.queue_snapshot = QueueSnapshot::read(&self.mpv);
        let clip_id = *self
            .queue_snapshot
            .ids
            .get(index)
            .ok_or("Could not queue the Live Photo clip")?;
        self.live = Some(LivePreview { photo_id, clip_id });
        let result = self.crossfade_to(index, clip_id);
        if result.is_err() {
            self.finish_live(false);
        }
        result
    }

    fn finish_live(&mut self, resume_photo: bool) {
        let Some(preview) = self.live.take() else {
            return;
        };
        let queue = QueueSnapshot::read(&self.mpv);
        if resume_photo && let Some(index) = queue.ids.iter().position(|id| *id == preview.photo_id)
        {
            let _ = self.crossfade_to(index, preview.photo_id);
        }
        if let Some(index) = queue.ids.iter().position(|id| *id == preview.clip_id) {
            let _ = self.mpv.command(&["playlist-remove", &index.to_string()]);
        }
        self.queue_snapshot = QueueSnapshot::read(&self.mpv);
    }

    fn play_index(&mut self, index: usize) -> Result<(), String> {
        let photo = self
            .media
            .playlist
            .get(index)
            .is_some_and(|path| image_path(path));
        let pending = if photo && self.media.kind == Kind::Image {
            self.mpv
                .get(&format!("playlist/{index}/id"))
                .as_i64()
                .map(|id| (id, index))
        } else {
            None
        };
        if !photo {
            self.stop_slideshow();
        }
        if self.slideshow.is_some()
            && let Some((id, _)) = pending
        {
            self.crossfade_to(index, id)?;
        } else {
            self.clear_crossfade();
            self.mpv
                .command(&["playlist-play-index", &index.to_string()])?;
            if self.timing.paused {
                self.mpv.set("pause", "no")?;
            }
        }
        self.photo_switch = pending;
        Ok(())
    }

    fn crossfade_to(&mut self, index: usize, id: i64) -> Result<(), String> {
        let display = self.timing.dimensions;
        self.fade = Crossfade::begin(&self.mpv, id, display, self.fade.take());
        if self.fade.is_none() {
            let _ = self.mpv.command(&["overlay-remove", "1"]);
        }
        if let Err(error) = self
            .mpv
            .command(&["playlist-play-index", &index.to_string()])
        {
            self.clear_crossfade();
            return Err(error);
        }
        self.mpv.set("pause", "no")
    }

    fn clear_crossfade(&mut self) {
        if self.fade.take().is_some() {
            let _ = self.mpv.command(&["overlay-remove", "1"]);
        }
    }

    fn choose(&mut self, picker: Picker) {
        if self.picker {
            return;
        }
        self.picker = true;
        let tx = self.tx.clone();
        let recent = self.recent.clone();
        thread::spawn(move || {
            #[cfg(target_os = "linux")]
            let response = {
                let mut dialog = dialog();
                match picker {
                    Picker::Files | Picker::Add => {
                        dialog
                            .args(["--multiple", "--separate-output", "--getopenfilename"])
                            .arg(&recent)
                            .arg(format!(
                                "{}|Media files\n*|All files",
                                platform::media_filter()
                            ));
                    }
                    Picker::Subtitles => {
                        dialog
                            .arg("--getopenfilename")
                            .arg(&recent)
                            .arg("*.srt *.vtt *.ass *.ssa *.sub|Subtitle files");
                    }
                    Picker::SlideshowMusic => {
                        dialog.arg("--getopenfilename").arg(&recent).arg(format!(
                            "{}|Audio files\n*|All files",
                            platform::audio_filter()
                        ));
                    }
                    Picker::Save => {
                        dialog
                            .arg("--getsavefilename")
                            .arg(recent.join("Playlist.coffee.json"))
                            .arg("*.coffee.json|Coffee playlists");
                    }
                    Picker::Load => {
                        dialog
                            .arg("--getopenfilename")
                            .arg(&recent)
                            .arg("*.coffee.json *.kahve.json *.json *.m3u *.m3u8|Playlists");
                    }
                }
                dialog
                    .output()
                    .ok()
                    .filter(|r| r.status.success())
                    .and_then(|r| String::from_utf8(r.stdout).ok())
                    .map(|s| s.trim_end_matches(['\n', '\r']).to_owned())
                    .filter(|s| !s.is_empty())
            };
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            let response = platform::pick(picker, &recent);
            let _ = tx.send(Input::Picked(picker, response));
        });
    }

    fn receive(&mut self, input: Input) {
        match input {
            Input::FontsLoaded(catalog) => self.fonts = catalog,
            #[cfg(unix)]
            Input::Launch(files) => {
                let _ = self.mpv.set("window-minimized", "no");
                if !files.is_empty() {
                    self.menu = None;
                    self.queue(&files, false, 0);
                    self.hovered = Instant::now();
                }
            }
            Input::Picked(picker, value) => {
                self.picker = false;
                self.hovered = Instant::now();
                let Some(value) = value else { return };
                if let Some(parent) = Path::new(value.lines().next().unwrap_or_default()).parent() {
                    self.recent = parent.to_path_buf();
                }
                match picker {
                    Picker::Files | Picker::Add => {
                        let files = value.lines().map(str::to_owned).collect::<Vec<_>>();
                        self.queue(&files, matches!(picker, Picker::Add), 0);
                    }
                    Picker::Subtitles => {
                        if let Err(e) = self.mpv.command(&["sub-add", &value, "select"]) {
                            self.note(e);
                        } else if let Some(id) = self.mpv.get("current-tracks/sub/id").as_i64()
                            && let Err(error) = self.subtitles.select(&self.mpv, id)
                        {
                            self.note(error);
                        }
                    }
                    Picker::SlideshowMusic => {
                        if let Some(path) =
                            library::normalize(&value).filter(|path| Path::new(path).is_file())
                        {
                            self.slideshow_music = Some(path);
                        }
                    }
                    Picker::Save => {
                        let playlist = library::Playlist {
                            files: self.media.playlist.clone(),
                            current: self.media.index.unwrap_or(0),
                        };
                        match library::save(Path::new(&value), &playlist) {
                            Ok(path) => self
                                .note(format!("Saved {}", display_name(&path.to_string_lossy()))),
                            Err(e) => self.note(e),
                        }
                    }
                    Picker::Load => match library::load(Path::new(&value)) {
                        Ok(playlist) => self.queue(&playlist.files, false, playlist.current),
                        Err(e) => self.note(e),
                    },
                }
            }
        }
    }

    fn action(&mut self, action: Action) {
        self.hovered = Instant::now();
        let result = match action {
            Action::Menu(menu) => {
                self.panel(menu);
                return;
            }
            Action::Escape => {
                if self.slideshow.is_some() {
                    self.stop_slideshow();
                    return;
                }
                if matches!(self.menu, Some(Menu::SubtitleEncoding | Menu::SubtitleFont)) {
                    self.menu = Some(Menu::Subtitles);
                    self.scroll = 0;
                    return;
                }
                if matches!(self.menu, Some(Menu::Decoder | Menu::Rendering)) {
                    self.menu = Some(Menu::Playback);
                    self.scroll = 0;
                    return;
                }
                if self.menu.take().is_some() {
                    return;
                }
                if self.timing.fullscreen {
                    self.mpv.set("fullscreen", "no")
                } else {
                    Ok(())
                }
            }
            Action::ClickStage => {
                if self.menu.take().is_some()
                    || self.media.kind == Kind::Empty
                    || self.media.kind == Kind::Image
                {
                    return;
                }
                self.mpv.command(&["cycle", "pause"])
            }
            Action::DragWindow | Action::ClickBar => {
                self.menu = None;
                Ok(())
            }
            Action::OpenContainingFolder => {
                self.menu = None;
                self.media.containing_folder().map_or(Ok(()), |folder| {
                    platform::open_target(folder.as_os_str())
                        .map_err(|e| format!("Couldn’t open the containing folder: {e}"))
                })
            }
            Action::ShowInPlaylist => {
                self.refresh_media();
                self.menu = Some(Menu::Playlist);
                // Starting at the active item keeps it visible even when
                // the window is only tall enough for a single playlist row.
                self.scroll = self.media.index.unwrap_or(0);
                return;
            }
            Action::OpenFiles
            | Action::AddFiles
            | Action::OpenSubtitles
            | Action::Save
            | Action::Load => {
                let picker = match action {
                    Action::OpenFiles => Picker::Files,
                    Action::AddFiles => Picker::Add,
                    Action::OpenSubtitles => Picker::Subtitles,
                    Action::Save => Picker::Save,
                    _ => Picker::Load,
                };
                self.menu = None;
                self.choose(picker);
                return;
            }
            Action::OpenStream => {
                self.menu = None;
                self.stream_dialog(false)
            }
            Action::SubmitStream => self.submit_stream(),
            Action::CancelStream => self.stream_dialog(true),
            Action::Play if self.media.kind == Kind::Video || self.media.kind == Kind::Music => {
                if self.menu == Some(Menu::Context) {
                    self.menu = None;
                }
                self.mpv.command(&["cycle", "pause"])
            }
            Action::Play => Ok(()),
            Action::Previous => {
                self.next(-1);
                return;
            }
            Action::LivePhoto => self.live_photo(),
            Action::StartPreparation => self.start_preparation(),
            Action::CancelPreparation => {
                if let Some(job) = &self.prepare_job {
                    job.cancel();
                }
                Ok(())
            }
            Action::PlayPrepared => self.play_prepared(),
            Action::PlayOriginal => self.play_original(),
            Action::PhotoZoom(zoom) => {
                if self.zoom != zoom {
                    self.photo_drag = None;
                    self.set_photo_pan([0.0; 2]);
                }
                self.zoom = zoom;
                self.mpv.photo_zoom(zoom, self.media.kind == Kind::Image)
            }
            Action::SlideshowMusic => {
                self.choose(Picker::SlideshowMusic);
                return;
            }
            Action::ClearSlideshowMusic => {
                self.slideshow_music = None;
                Ok(())
            }
            Action::StartSlideshow => self.start_slideshow(),
            Action::StopSlideshow => {
                self.stop_slideshow();
                Ok(())
            }
            Action::Next => {
                self.next(1);
                return;
            }
            Action::Skip(seconds) => {
                if self.media.kind == Kind::Image {
                    self.next(seconds.signum());
                    return;
                }
                if self.media.kind == Kind::Empty {
                    return;
                }
                self.playback_health.interrupt();
                self.mpv.command(&[
                    "seek",
                    &seconds.to_string(),
                    if self.timing.reverse {
                        "relative+exact"
                    } else {
                        "relative"
                    },
                ])
            }
            Action::Seek(part) => {
                self.playback_health.interrupt();
                self.mpv
                    .command(&["seek", &(part * 100.0).to_string(), "absolute-percent"])
            }
            Action::RepeatOne => {
                if matches!(self.media.kind, Kind::Video | Kind::Music) && self.live.is_none() {
                    self.mpv
                        .command(&["cycle-values", "loop-file", "no", "inf"])
                } else {
                    Ok(())
                }
            }
            Action::Mute => {
                let audio = self.audio_controls();
                if audio.get("volume").as_f64() == Some(0.0) {
                    audio
                        .set("volume", &self.last_volume.to_string())
                        .and_then(|()| audio.set("mute", "no"))
                } else {
                    audio.command(&["cycle", "mute"])
                }
            }
            Action::Volume(delta) => {
                if self.menu.is_some() {
                    self.scroll = self
                        .scroll
                        .saturating_add_signed(-(delta.signum() as isize));
                    return;
                }
                self.audio_controls()
                    .command(&["add", "volume", &delta.to_string()])
            }
            Action::SetVolume(percent) => self
                .audio_controls()
                .set("volume", &percent.to_string())
                .and_then(|()| self.audio_controls().set("mute", "no"))
                .map(|()| {
                    if percent > 0 {
                        self.last_volume = f64::from(percent);
                    }
                }),
            Action::ToggleSubtitles => {
                self.subtitles.toggle();
                Ok(())
            }
            Action::SubtitleDirection(direction) => {
                self.subtitles.set_direction(direction);
                Ok(())
            }
            Action::SubtitleEncoding(encoding) => {
                self.menu = Some(Menu::Subtitles);
                self.scroll = 0;
                self.subtitles.set_encoding(&self.mpv, encoding)
            }
            Action::SubtitleFont(index) => {
                let font = match index {
                    None => None,
                    Some(i) => match &self.fonts {
                        fonts::Catalog::Ready(names) => {
                            let Some(family) = names.get(i) else { return };
                            Some(family.as_str())
                        }
                        _ => return,
                    },
                };
                self.menu = Some(Menu::Subtitles);
                self.scroll = 0;
                self.subtitles.set_font(&self.mpv, font)
            }
            Action::Audio(id) => self.mpv.set("aid", &id.to_string()),
            Action::Subtitles(id) => self.subtitles.select(&self.mpv, id),
            Action::Speed(step) => self.step_playback_speed(step),
            Action::Quality(bitrate) => {
                self.quality = bitrate;
                self.mpv.set("hls-bitrate", bitrate).and_then(|_| {
                    if let Some(i) = self.media.index {
                        self.mpv.command(&["playlist-play-index", &i.to_string()])
                    } else {
                        Ok(())
                    }
                })
            }
            Action::Hdr(mode) => mode.apply(&self.mpv).map(|()| {
                self.hdr = mode;
            }),
            Action::Decode(choice) => {
                if self.timing.reverse {
                    self.note("Switch to forward playback to change the video decoder.");
                    return;
                }
                if self.decoder == choice && env::var_os("KAHVE_HWDEC").is_none() {
                    return;
                }
                self.devices.apply(&self.mpv, choice).map(|()| {
                    self.decoder = choice;
                    if let Err(error) = self.devices.save(choice) {
                        self.note(format!("Couldn’t save decoder preference: {error}"));
                    }
                })
            }
            Action::Render(mode) => {
                self.render_mode = mode;
                self.update_render_profile()
            }
            Action::Fullscreen => {
                self.menu = None;
                if self.slideshow.is_some() {
                    self.stop_slideshow();
                    Ok(())
                } else {
                    self.mpv.command(&["cycle", "fullscreen"])
                }
            }
            Action::Minimize => self.mpv.set("window-minimized", "yes"),
            Action::Maximize => self.mpv.command(&["cycle", "window-maximized"]),
            Action::Quit => self.mpv.command(&["quit"]),
            Action::Clear => {
                self.stop_slideshow();
                self.photo_switch = None;
                let _ = self.mpv.command(&["playlist-clear"]);
                self.mpv.command(&["stop"])
            }
            Action::Jump(index) => self.play_index(index),
            Action::Remove(index) => self.mpv.command(&["playlist-remove", &index.to_string()]),
            Action::Scroll(delta) => {
                self.scroll = self.scroll.saturating_add_signed(delta as isize);
                return;
            }
            Action::Link(url) => {
                let _ = platform::open_target(std::ffi::OsStr::new(url));
                return;
            }
        };
        if let Err(error) = result {
            self.note(error);
        }
    }

    fn finish_single_click(&mut self) {
        if self
            .pending_click
            .take()
            .is_some_and(|click| click.area == ClickArea::Video)
            && self.menu.is_none()
        {
            self.action(Action::ClickStage);
        }
    }

    fn click(&mut self, action: Action, point: (i32, i32)) {
        let area = match action {
            Action::ClickStage if matches!(self.media.kind, Kind::Video | Kind::Image) => {
                Some(ClickArea::Video)
            }
            Action::DragWindow if !self.timing.fullscreen => Some(ClickArea::TopBar),
            Action::ClickBar if !self.timing.fullscreen => Some(ClickArea::BottomBar),
            _ => None,
        };
        if let Some(area) = area.filter(|_| self.menu.is_none()) {
            let now = Instant::now();
            if self
                .pending_click
                .as_ref()
                .is_some_and(|click| click.matches(area, point, now))
            {
                self.pending_click = None;
                self.action(if area == ClickArea::Video {
                    Action::Fullscreen
                } else {
                    Action::Maximize
                });
            } else {
                self.finish_single_click();
                self.pending_click = Some(PendingClick {
                    area,
                    point,
                    at: now,
                });
            }
        } else {
            self.pending_click = None;
            self.action(action);
        }
    }

    fn maybe_drag_window(&mut self, mouse: Option<(i32, i32)>) {
        let (Some(origin), Some(point)) = (self.drag_origin, mouse) else {
            return;
        };
        if origin.0.abs_diff(point.0).max(origin.1.abs_diff(point.1)) < 5 {
            return;
        }
        // The compositor needs the pressed button's serial. Wait for motion
        // rather than grabbing it on press (which consumes a stationary click).
        self.drag_origin = None;
        self.pressed = None;
        self.pending_click = None;
        self.menu = None;
        let _ = self.mpv.command(&["begin-vo-dragging"]);
    }

    fn dragging_volume(&self) -> bool {
        matches!(self.pressed, Some(Action::SetVolume(_)))
    }

    fn drag_volume_to(&mut self, mouse: Option<(i32, i32)>) {
        let Some(Action::SetVolume(last)) = self.pressed else {
            return;
        };
        if let Some(percent) = mouse.and_then(|point| self.frame.as_ref()?.volume_at(point, true))
            && percent != last
        {
            self.pressed = Some(Action::SetVolume(percent));
            self.action(Action::SetVolume(percent));
        }
    }

    fn set_photo_pan(&mut self, position: [f64; 2]) {
        for (i, key) in ["video-pan-x", "video-pan-y"].into_iter().enumerate() {
            if position[i] != self.photo_pan[i]
                && self
                    .mpv
                    .set(
                        &format!("file-local-options/{key}"),
                        &position[i].to_string(),
                    )
                    .is_ok()
            {
                self.photo_pan[i] = position[i];
            }
        }
    }

    fn drag_photo_to(&mut self, mouse: Option<(i32, i32)>) {
        let (Some(drag), Some(point)) = (&mut self.photo_drag, mouse) else {
            return;
        };
        let delta = [
            f64::from(point.0) - f64::from(drag.origin.0),
            f64::from(point.1) - f64::from(drag.origin.1),
        ];
        if !drag.moved && delta[0].abs().max(delta[1].abs()) < 5.0 {
            return;
        }
        drag.moved = true;
        self.pending_click = None;
        let position = drag.bounds.drag(drag.position, delta);
        self.set_photo_pan(position);
    }

    fn pointer(&mut self, event: PointerEvent) {
        self.hovered = Instant::now();
        let mouse = mouse_position(&self.mpv);
        let target = mouse.map(|point| {
            self.frame
                .as_ref()
                .map_or(Action::ClickStage, |f| f.at(point))
        });
        match event {
            PointerEvent::Down => {
                self.photo_drag = None;
                match target {
                    Some(action @ Action::SetVolume(_)) => {
                        self.pending_click = None;
                        self.pressed = Some(action.clone());
                        self.action(action);
                    }
                    Some(Action::ClickStage)
                        if self.media.kind == Kind::Image
                            && self.photo_switch.is_none()
                            && self.menu.is_none()
                            && self.stream.is_none() =>
                    {
                        self.drag_origin = None;
                        self.photo_drag = mouse.map(|origin| PhotoDrag {
                            origin,
                            bounds: self.timing.pan_bounds,
                            position: self.photo_pan,
                            moved: false,
                        });
                        self.pressed = Some(Action::ClickStage);
                    }
                    _ => {
                        self.drag_origin =
                            if target == Some(Action::DragWindow) && !self.timing.fullscreen {
                                mouse
                            } else {
                                None
                            };
                        self.pressed = target;
                    }
                }
            }
            PointerEvent::Up => {
                self.drag_origin = None;
                self.drag_photo_to(mouse);
                if self.photo_drag.take().is_some_and(|drag| drag.moved) {
                    self.pressed = None;
                    if let Some(slideshow) = &mut self.slideshow {
                        slideshow.next = Instant::now() + SLIDE_INTERVAL;
                    }
                    return;
                }
                if self.dragging_volume() {
                    self.drag_volume_to(mouse);
                    self.pressed = None;
                } else if let (Some(down), Some(up), Some(point)) =
                    (self.pressed.take(), target, mouse)
                    && (down == up || matches!((&down, &up), (Action::Seek(_), Action::Seek(_))))
                {
                    self.click(up, point);
                }
            }
            PointerEvent::Click => {
                if let (Some(action), Some(point)) = (target, mouse) {
                    self.click(action, point);
                }
            }
            PointerEvent::Cancel => {
                self.pressed = None;
                self.drag_origin = None;
                self.photo_drag = None;
            }
        }
    }

    fn handle_event(&mut self, event: Event, metadata: &mut Instant) -> bool {
        match event {
            Event::Shutdown => return false,
            Event::Start(id) => {
                self.active_file = Some(id);
                self.reverse_hwdec = None;
                self.seek_preview.source(&self.mpv, None);
                self.prepare_target = None;
                self.playback_health = prepare::PlaybackHealth::default();
                self.prepare_job = None;
                self.prepared_track = None;
                if self.menu == Some(Menu::Prepare) {
                    self.menu = None;
                }
                self.photo_drag = None;
                self.photo_pan = [0.0; 2];
                if self.fade.as_ref().is_some_and(|fade| fade.target != id)
                    && QueueSnapshot::read(&self.mpv).current == Some(id)
                {
                    self.clear_crossfade();
                }
                if self.live.as_ref().is_some_and(|live| live.clip_id != id) {
                    self.finish_live(false);
                }
                self.pending_click = None;
                self.refresh_media();
                *metadata = Instant::now();
                self.hovered = Instant::now();
            }
            Event::Loaded => {
                if let Some((target, _)) = self.photo_switch {
                    if self.active_file != Some(target) {
                        return true;
                    }
                    self.photo_switch = None;
                }
                self.pending_click = None;
                self.refresh_media();
                self.preview_original();
                if self.media.kind == Kind::Video && self.live.is_none() {
                    let av1 = self.mpv.get("current-tracks/video/codec").as_str() == Some("av1");
                    self.prepare_target = library::normalize(&self.media.path).and_then(|path| {
                        prepare::Target::local(
                            &path,
                            self.media.source_range.starts_with("HDR"),
                            av1,
                        )
                    });
                }
                if self.live.is_none() {
                    self.prepare_photos();
                    if self.photos.is_none() {
                        self.stop_slideshow();
                        if self.menu == Some(Menu::Slideshow) {
                            self.menu = None;
                        }
                    }
                }
                *metadata = Instant::now();
                self.hovered = Instant::now();
            }
            Event::End { id, eof, error } => {
                if (eof || error.is_some())
                    && self.live.as_ref().is_some_and(|live| live.clip_id == id)
                {
                    self.finish_live(true);
                }
                if let Some(error) = error {
                    if self.photo_switch.is_some_and(|(target, _)| target == id) {
                        self.photo_switch = None;
                        self.stop_slideshow();
                    }
                    if self.fade.as_ref().is_some_and(|fade| fade.target == id) {
                        self.clear_crossfade();
                    }
                    self.note(format!("Couldn’t play this item: {error}"));
                }
                // A normal end usually precedes another Start. Polling on
                // the next frame is enough if playback actually went idle.
            }
            Event::FrameReady => {
                if let (Some(fade), Some(id)) = (&mut self.fade, self.active_file) {
                    fade.ready(id, Instant::now());
                }
                if let Some(slideshow) = &mut self.slideshow
                    && self.photo_switch.is_none()
                    && self.media.kind == Kind::Image
                {
                    slideshow.next = Instant::now() + SLIDE_INTERVAL;
                }
            }
            Event::Pointer(event) => self.pointer(event),
            Event::PlaylistChanged => self.playlist_changed = Some(Instant::now()),
            Event::TextInput(text) => {
                if self.menu == Some(Menu::SubtitleFont) && self.stream.is_none() {
                    self.type_font_query(&text);
                } else if let Some(input) = &mut self.stream {
                    input.insert(&text);
                }
            }
            Event::Action(name) => {
                self.pending_click = None;
                if name.starts_with("stream-") {
                    self.stream_key(&name);
                    return true;
                }
                if name.starts_with("font-") {
                    self.font_key(&name);
                    return true;
                }
                let action = match name.as_str() {
                    "context" => {
                        if let Some(point) = mouse_position(&self.mpv) {
                            self.hovered = Instant::now();
                            self.context_anchor = Some(point);
                            self.menu = Some(Menu::Context);
                            self.scroll = 0;
                        }
                        return true;
                    }
                    "play" => Action::Play,
                    "left" => Action::Skip(-10),
                    "right" => Action::Skip(10),
                    "fullscreen" => Action::Fullscreen,
                    "previous" => Action::Previous,
                    "next" => Action::Next,
                    "volume-up" => Action::Volume(5),
                    "volume-down" => Action::Volume(-5),
                    "mute" => Action::Mute,
                    "captions" => Action::ToggleSubtitles,
                    "open-files" => Action::OpenFiles,
                    "playlist" => Action::Menu(Menu::Playlist),
                    "media-info" => Action::Menu(Menu::Info),
                    "shortcuts" => Action::Menu(Menu::Shortcuts),
                    "escape" => Action::Escape,
                    _ => return true,
                };
                self.action(action);
                self.refresh_media();
                *metadata = Instant::now();
            }
            Event::Changed => {}
        }
        true
    }

    fn run(mut self) {
        let wayland = env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
        let mut last_metadata = Instant::now();
        let mut last_frame = Instant::now() - Duration::from_secs(1);
        #[cfg(windows)]
        let mut dark_hwnd = None;
        loop {
            let interval = if self.fade.is_some()
                || self.controls.active(Instant::now())
                || self.dragging_volume()
                || self.photo_drag.is_some()
                || self.seek_preview.hovering()
            {
                Duration::from_millis(16)
            } else {
                Duration::from_millis(30)
            };
            let wait = interval.saturating_sub(last_frame.elapsed());
            if let Some(event) = self.mpv.event(wait.as_secs_f64())
                && !self.handle_event(event, &mut last_metadata)
            {
                break;
            }
            while let Ok(input) = self.rx.try_recv() {
                self.receive(input);
            }
            self.play_dropped_files();
            self.sync_font_input();
            if self
                .pending_click
                .as_ref()
                .is_some_and(|click| click.at.elapsed() >= DOUBLE_CLICK)
            {
                self.finish_single_click();
            }
            if last_frame.elapsed() < interval {
                continue;
            }
            last_frame = Instant::now();
            self.tick_slideshow();
            if last_metadata.elapsed() > Duration::from_millis(250) {
                self.refresh_media();
                last_metadata = Instant::now();
            }
            let timing = Timings::read(&self.mpv);
            if self.live.as_ref().is_some_and(|preview| {
                self.mpv.get("eof-reached").as_bool() == Some(true)
                    && self
                        .mpv
                        .get("playlist-current-pos")
                        .as_u64()
                        .and_then(|index| self.mpv.get(&format!("playlist/{index}/id")).as_i64())
                        == Some(preview.clip_id)
            }) {
                self.finish_live(true);
            }
            if timing.volume > 0.0 {
                self.last_volume = timing.volume;
            }
            let (w, h) = timing.dimensions;
            let resize = timing.dimensions != self.timing.dimensions;
            let backing_scale = timing.backing_scale;
            let rescale = backing_scale != self.timing.backing_scale;
            if timing.mouse.is_some() && timing.mouse != self.timing.mouse {
                let now = Instant::now();
                self.hovered = now;
                if let Some(slideshow) = &mut self.slideshow
                    && !resize
                    && now >= slideshow.reveal_after
                {
                    slideshow.controls_until = Some(now + SLIDE_CONTROLS_TIMEOUT);
                }
            }
            self.drag_volume_to(timing.mouse);
            if resize {
                self.photo_drag = None;
                if self.media.kind == Kind::Image && self.photo_switch.is_none() {
                    self.set_photo_pan(timing.pan_bounds.clamp(self.photo_pan));
                }
            } else {
                self.drag_photo_to(timing.mouse);
            }
            self.maybe_drag_window(timing.mouse);
            self.timing = timing;
            if w <= 0 || h <= 0 {
                continue;
            }
            if self
                .fade
                .as_mut()
                .is_some_and(|fade| !fade.draw(&self.mpv, (w, h), Instant::now()))
            {
                self.clear_crossfade();
            }
            #[cfg(windows)]
            if (resize || dark_hwnd.is_none())
                && let Some(hwnd) = self.mpv.get("window-id").as_i64().filter(|id| *id != 0)
            {
                platform::dark_windows_frame(hwnd);
                dark_hwnd = Some(hwnd);
            }
            let now = Instant::now();
            self.sample_playback(now);
            let show_controls = self.menu.is_some()
                || self.stream.is_some()
                || if let Some(slideshow) = &self.slideshow {
                    self.photo_drag.is_some()
                        || slideshow.controls_until.is_some_and(|until| now < until)
                } else {
                    self.media.kind != Kind::Video
                        || self.timing.paused
                        || self.dragging_volume()
                        || self.hovered.elapsed() < Duration::from_secs(3)
                };
            let controls_opacity = self.controls.update(show_controls, Instant::now());
            let subtitle_position = ui::subtitle_position(
                (w, h),
                backing_scale,
                self.media.kind == Kind::Video && controls_opacity > 0,
            );
            let caption = self.subtitles.sync(&self.mpv, subtitle_position);
            let prepare_status = self.prepare_job.as_ref().map(prepare::Job::status);
            let hover = (show_controls
                && controls_opacity > 0
                && self.menu.is_none()
                && self.stream.is_none()
                && self.media.kind == Kind::Video
                && self.timing.seekable
                && self.timing.duration > 0.0)
                .then(|| {
                    self.timing
                        .mouse
                        .and_then(|mouse| ui::seek_hover((w, h), backing_scale, mouse))
                })
                .flatten();
            let preview_allowed = !matches!(prepare_status, Some(prepare::Status::Running(..)))
                && self.timing.seekable;
            let request = hover.map(|hover| {
                (
                    (hover.fraction * self.timing.duration)
                        .min((self.timing.duration - 0.1).max(0.0)),
                    hover.image_bounds,
                )
            });
            let has_image =
                self.seek_preview
                    .update(&self.mpv, request, self.timing.duration, preview_allowed);
            let frame = ui::draw(View {
                w,
                h,
                backing_scale,
                mouse: self.timing.mouse.unwrap_or((-100, -100)),
                media: &self.media,
                photo_position: self
                    .photos
                    .as_ref()
                    .filter(|_| self.media.kind == Kind::Image)
                    .map(|folder| (folder.index, folder.files().len())),
                live_available: self.media.kind == Kind::Image
                    && self
                        .photos
                        .as_ref()
                        .is_some_and(|folder| folder.live_clip().is_some()),
                live_playing: self.live.is_some(),
                timeline: hover.map(|hover| ui::TimelineView {
                    hover,
                    seconds: hover.fraction * self.timing.duration,
                    has_image,
                }),
                prepare: self
                    .prepare_target
                    .as_ref()
                    .filter(|_| self.playback_health.detected())
                    .map(|target| ui::PrepareView {
                        cached: target.cached,
                        legacy: target.legacy.is_some(),
                        playing: self.prepared_track.is_some(),
                        file_name: target
                            .output
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("the prepared MP4"),
                        status: prepare_status.as_ref(),
                    }),
                caption: caption.as_deref(),
                caption_style: self.subtitles.style,
                subtitle_direction: self.subtitles.direction,
                subtitle_encoding: self.subtitles.encoding,
                subtitle_font: self.subtitles.font.as_deref(),
                font_catalog: &self.fonts,
                font_query: &self.font_query,
                photo_zoom: self.zoom,
                slideshow_playing: self.slideshow.is_some(),
                slideshow_music: self.slideshow_music.as_deref(),
                picking_music: self.picker,
                stream: self.stream.as_ref().map(|input| ui::StreamView {
                    before: &input.text[..input.cursor],
                    after: &input.text[input.cursor..],
                    all: input.all,
                    invalid: input.invalid,
                }),
                paused: self.timing.paused,
                muted: self.timing.muted,
                repeat_one: self.timing.repeat_one,
                fullscreen: self.timing.fullscreen,
                native_titlebar: (wayland || cfg!(any(windows, target_os = "macos")))
                    && self.timing.border,
                resize_grips: wayland
                    && !self.timing.border
                    && !self.timing.fullscreen
                    && !self.timing.maximized,
                controls_interactive: show_controls,
                controls_opacity,
                buffering: self.timing.buffering
                    && self.photo_switch.is_none()
                    && self.slideshow.is_none(),
                volume: self.timing.volume,
                position: self.timing.position,
                duration: self.timing.duration,
                buffered: self.timing.buffered,
                menu: self.menu,
                context_anchor: self.context_anchor,
                scroll: self.scroll,
                hdr: self.hdr,
                devices: &self.devices.decoders,
                decoder: self.decoder,
                render_mode: self.render_mode,
                fast_render: self.fast_render,
                speed: if self.timing.reverse {
                    -self.timing.speed
                } else {
                    self.timing.speed
                },
                seekable: self.timing.seekable,
                quality: self.quality,
                toast: if self.toast.1.elapsed() < Duration::from_secs(4) {
                    &self.toast.0
                } else {
                    ""
                },
            });
            if resize
                || rescale
                || self
                    .frame
                    .as_ref()
                    .is_none_or(|old| old.osd() != frame.osd())
            {
                let (w, h) = frame.resolution();
                let _ = self.mpv.command(&[
                    "osd-overlay",
                    "2",
                    "ass-events",
                    frame.osd(),
                    &w.to_string(),
                    &h.to_string(),
                ]);
            }
            self.frame = Some(frame);
        }
    }
}

fn smoke(path: &str, cached: bool) -> Result<(), String> {
    let mpv = Engine::create(None, true, None)?;
    let path = library::normalize(path).ok_or("Not a playable path")?;
    if cached {
        let source = Path::new(&path);
        if !source.is_file() {
            return Err("--preload needs a local file".into());
        }
        mpv.preload(vec![source.to_path_buf()]);
        let deadline = Instant::now() + Duration::from_secs(7);
        while !mpv.is_preloaded(source) {
            if Instant::now() >= deadline {
                return Err("Timed out waiting for the RAM preload".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    mpv.command(&["loadfile", &path])?;
    let start = Instant::now();
    let mut loaded = false;
    while start.elapsed() < Duration::from_secs(7) {
        match mpv.event(0.2) {
            Some(Event::Loaded) => loaded = true,
            Some(Event::End {
                error: Some(error), ..
            }) => return Err(error),
            _ => {}
        }
        if loaded && (mpv.get("video-params").is_object() || mpv.get("audio-params").is_object()) {
            if cached
                && !mpv
                    .get("stream-open-filename")
                    .as_str()
                    .is_some_and(|p| p.starts_with("coffee-ram://"))
            {
                return Err("libmpv did not open the preloaded RAM stream".into());
            }
            let media = Media::read(&mpv);
            println!(
                "Loaded: {}\nlibmpv: {}",
                media.title,
                mpv.get("mpv-version")
            );
            for (label, value) in &media.infos {
                println!("{label}: {value}");
            }
            // Test that the installed renderer options can be changed at runtime.
            Hdr::Prefer.apply(&mpv)?;
            Hdr::Off.apply(&mpv)?;
            Hdr::Auto.apply(&mpv)?;
            RenderMode::apply(true, &mpv)?;
            RenderMode::apply(false, &mpv)?;
            return Ok(());
        }
    }
    Err("Timed out waiting for a decoded video frame or audio output".into())
}

fn entry() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();
    if first.as_deref().is_some_and(|x| x == "--smoke-test") {
        let second = arguments.next().ok_or("Provide a file for --smoke-test")?;
        let cached = second == "--preload";
        let path = if cached {
            arguments
                .next()
                .ok_or("Provide a file for --smoke-test --preload")?
        } else {
            second
        };
        return smoke(&path.to_string_lossy(), cached);
    }
    if first.as_deref().is_some_and(|x| x == "--version") {
        println!("Coffee {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let files = first
        .into_iter()
        .chain(arguments)
        .filter_map(|x| library::normalize(&x.to_string_lossy()))
        .collect::<Vec<_>>();
    let (tx, rx) = mpsc::channel();
    let Some(runtime) = runtime(&files, tx.clone())? else {
        return Ok(());
    };
    let input = runtime.dir.join("input.conf");
    fs::write(&input, include_str!("../assets/input.conf")).map_err(|e| e.to_string())?;
    platform::ensure_dark_frame();
    let devices = Devices::discover();
    let mpv = Engine::create(Some(&input), false, devices.renderer.as_deref())?;
    mpv.watch_playlist()?;
    mpv.command(&[
        "define-section",
        "coffee-subtitle-font",
        include_str!("../assets/subtitle-font-input.conf"),
        "force",
    ])?;
    let subtitles = subtitles::Subtitles::new(&mpv)?;
    let decoder = if env::var_os("KAHVE_HWDEC").is_some() {
        DecodeChoice::Auto
    } else {
        devices.saved()
    };
    if decoder != DecodeChoice::Auto {
        devices.apply(&mpv, decoder)?;
    }
    let recent = env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(env::temp_dir);
    let mut app = App {
        mpv,
        tx,
        rx,
        media: Media::default(),
        timing: Timings::default(),
        frame: None,
        menu: None,
        context_anchor: None,
        scroll: 0,
        hdr: Hdr::Auto,
        devices,
        decoder,
        render_mode: RenderMode::Auto,
        fast_render: false,
        quality: "max",
        toast: (String::new(), Instant::now()),
        hovered: Instant::now(),
        controls: ControlsFade::new(Instant::now()),
        pressed: None,
        drag_origin: None,
        photo_drag: None,
        photo_pan: [0.0; 2],
        pending_click: None,
        last_volume: 100.0,
        picker: false,
        recent,
        fade: None,
        active_file: None,
        photo_switch: None,
        zoom: PhotoZoom::Fit,
        slideshow: None,
        slideshow_music: None,
        photos: None,
        live: None,
        stream: None,
        subtitles,
        fonts: fonts::Catalog::default(),
        font_query: String::new(),
        font_input: false,
        prepare_target: None,
        prepare_job: None,
        prepared_track: None,
        playback_health: prepare::PlaybackHealth::default(),
        reverse_hwdec: None,
        seek_preview: seek_preview::Preview::default(),
        queue_snapshot: QueueSnapshot::default(),
        playlist_changed: None,
    };
    if !files.is_empty() {
        app.queue(&files, false, 0);
    }
    app.run();
    Ok(())
}

#[cfg(target_os = "linux")]
fn dialog() -> Command {
    let mut command = Command::new("kdialog");
    let scale = env::var("QT_SCALE_FACTOR")
        .ok()
        .and_then(|n| n.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(1.0);
    command.env(
        "QT_SCALE_FACTOR",
        (scale * f64::from(crate::scale::PERCENT) / 100.0).to_string(),
    );
    command.env(
        "KDE_COLOR_SCHEME_PATH",
        "/usr/share/color-schemes/BreezeDark.colors",
    );
    command.args(["--title", "Coffee"]);
    command
}

fn run_entry() {
    if let Err(error) = entry() {
        eprintln!("Coffee: {error}");
        if !env::args_os().any(|arg| arg == "--smoke-test" || arg == "--version") {
            platform::error_dialog(&error);
        }
        std::process::exit(1);
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    if !env::args_os().any(|arg| arg == "--smoke-test" || arg == "--version") {
        platform::cocoa_loop(run_entry);
        return;
    }
    run_entry();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_images_in_natural_order() {
        use photos::natural_order;
        assert!(natural_order("Photo 2.jpg", "Photo 10.jpg").is_lt());
        assert!(natural_order("001.png", "100.png").is_lt());
    }

    #[test]
    fn controls_fade_in_and_out_over_200_ms() {
        let start = Instant::now();
        let mut fade = ControlsFade::new(start);
        assert_eq!(fade.update(false, start), 255);
        assert_eq!(fade.update(false, start + Duration::from_millis(100)), 128);
        assert_eq!(fade.update(false, start + CONTROLS_FADE), 0);
        let show = start + Duration::from_secs(1);
        assert_eq!(fade.update(true, show), 0);
        assert_eq!(fade.update(true, show + Duration::from_millis(100)), 128);
        assert_eq!(fade.update(true, show + CONTROLS_FADE), 255);
    }

    #[test]
    fn controls_fade_can_reverse_without_jumping() {
        let start = Instant::now();
        let mut fade = ControlsFade::new(start);
        fade.update(false, start);
        let reverse = start + Duration::from_millis(60);
        assert_eq!(fade.update(true, reverse), 179);
        assert_eq!(fade.update(true, reverse + CONTROLS_FADE), 255);
    }

    #[test]
    fn double_click_requires_the_same_surface_time_and_position() {
        let at = Instant::now();
        let first = PendingClick {
            area: ClickArea::Video,
            point: (400, 200),
            at,
        };
        let quick = at + Duration::from_millis(140);
        assert!(first.matches(ClickArea::Video, (407, 205), quick));
        assert!(!first.matches(ClickArea::Video, (410, 205), quick));
        assert!(!first.matches(ClickArea::TopBar, (400, 200), quick));
        assert!(!first.matches(ClickArea::Video, (400, 200), at + DOUBLE_CLICK));
    }

    #[test]
    fn a_native_drop_selects_the_first_new_id_without_replacing_the_queue() {
        let before = QueueSnapshot {
            ids: vec![1, 2],
            current: Some(1),
        };
        let batch = QueueSnapshot {
            ids: vec![1, 2, 3, 4],
            current: Some(1),
        };
        assert_eq!(before.first_drop(&batch), Some(2));
        assert_eq!(batch.first_drop(&batch), None);
        assert_eq!(QueueSnapshot::default().first_drop(&batch), None);
        assert_eq!(
            before.first_drop(&QueueSnapshot {
                ids: vec![3, 4],
                current: Some(3)
            }),
            None
        );
        assert_eq!(
            before.first_drop(&QueueSnapshot {
                ids: vec![1, 3, 4],
                current: Some(1)
            }),
            None
        );
        assert_eq!(
            before.first_drop(&QueueSnapshot {
                ids: vec![1, 2, 3],
                current: Some(2)
            }),
            None
        );
    }
}
