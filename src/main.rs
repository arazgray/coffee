#![cfg_attr(windows, windows_subsystem = "windows")]

mod engine;
mod gpu;
mod library;
mod photos;
mod platform;
mod scale;
mod state;
mod ui;

use engine::{Engine, Event, PointerEvent};
use gpu::{DecodeChoice, Devices};
use photos::PhotoFolder;
use state::{Hdr, Kind, Media, RenderMode, display_name, image_path, is_stream};
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
    Subtitles,
    Save,
    Load,
}

enum Input {
    #[cfg(unix)]
    Launch(Vec<String>),
    Picked(Picker, Option<String>),
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
    fullscreen: bool,
    maximized: bool,
    border: bool,
    buffering: bool,
    duration: f64,
    position: f64,
    buffered: f64,
    volume: f64,
    speed: f64,
    dimensions: (i32, i32),
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
        Self {
            paused: mpv.get("pause").as_bool().unwrap_or(true),
            muted: mpv.get("mute").as_bool().unwrap_or(false),
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
            dimensions: (
                dimensions["w"].as_i64().unwrap_or_default() as i32,
                dimensions["h"].as_i64().unwrap_or_default() as i32,
            ),
            mouse: mouse_position(mpv),
        }
    }
}

struct Fade {
    start: Instant,
    target: usize,
    switched: bool,
}

struct LivePreview {
    photo_id: i64,
    clip_id: i64,
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
    pending_click: Option<PendingClick>,
    last_volume: f64,
    picker: bool,
    recent: PathBuf,
    fade: Option<Fade>,
    photos: Option<PhotoFolder>,
    live: Option<LivePreview>,
    stream: Option<StreamInput>,
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
            "stream-cancel" => self.stream_dialog(true),
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
        self.media = Media::read(&self.mpv);
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
            self.fade = None;
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
    }

    fn next(&mut self, direction: i32) {
        if self.media.kind == Kind::Image
            && let Some(folder) = &self.photos
        {
            let Some(path) = folder
                .adjacent(direction)
                .map(|p| p.to_string_lossy().into_owned())
            else {
                return;
            };
            let existing = self
                .media
                .playlist
                .iter()
                .position(|entry| library::normalize(entry).is_some_and(|entry| entry == path));
            let index = match existing {
                Some(index) => index,
                None => {
                    let index = QueueSnapshot::read(&self.mpv).ids.len();
                    if let Err(error) = self.mpv.command(&["loadfile", &path, "append"]) {
                        self.note(error);
                        return;
                    }
                    self.queue_snapshot = QueueSnapshot::read(&self.mpv);
                    self.refresh_media();
                    index
                }
            };
            self.jump(index);
            return;
        }
        let Some(index) = self.media.index else {
            return;
        };
        let next = (index as i64 + i64::from(direction))
            .try_into()
            .ok()
            .filter(|i: &usize| *i < self.media.playlist.len());
        if let Some(index) = next {
            self.jump(index);
        } else if self.media.kind == Kind::Image {
            self.note(if direction < 0 {
                "First image in the folder."
            } else {
                "Last image in the folder."
            });
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
            .and_then(|folder| folder.live_clip.as_ref())
            .filter(|_| self.media.kind == Kind::Image)
            .ok_or("No Live Photo clip")?;
        let queue = QueueSnapshot::read(&self.mpv);
        let photo_id = queue.current.ok_or("No active photo")?;
        let index = queue.ids.len();
        self.mpv
            .command(&["loadfile", &clip.to_string_lossy(), "append"])?;
        self.queue_snapshot = QueueSnapshot::read(&self.mpv);
        let clip_id = *self
            .queue_snapshot
            .ids
            .get(index)
            .ok_or("Could not queue the Live Photo clip")?;
        self.live = Some(LivePreview { photo_id, clip_id });
        self.fade = None;
        let result = self
            .mpv
            .command(&["playlist-play-index", &index.to_string()]);
        if result.is_err() {
            self.finish_live(false);
        } else {
            let _ = self.mpv.set("pause", "no");
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
            let _ = self
                .mpv
                .command(&["playlist-play-index", &index.to_string()]);
            let _ = self.mpv.set("pause", "no");
        }
        if let Some(index) = queue.ids.iter().position(|id| *id == preview.clip_id) {
            let _ = self.mpv.command(&["playlist-remove", &index.to_string()]);
        }
        self.queue_snapshot = QueueSnapshot::read(&self.mpv);
    }

    fn jump(&mut self, index: usize) {
        if self.media.kind == Kind::Image
            && self
                .media
                .playlist
                .get(index)
                .is_some_and(|path| image_path(path))
            && Some(index) != self.media.index
        {
            self.fade = Some(Fade {
                start: Instant::now(),
                target: index,
                switched: false,
            });
        } else {
            let _ = self
                .mpv
                .command(&["playlist-play-index", &index.to_string()]);
            let _ = self.mpv.set("pause", "no");
        }
    }

    fn fade_opacity(&mut self) -> f64 {
        let Some(fade) = &mut self.fade else {
            return 0.0;
        };
        let elapsed = fade.start.elapsed().as_secs_f64();
        if elapsed >= 0.105 && !fade.switched {
            let _ = self
                .mpv
                .command(&["playlist-play-index", &fade.target.to_string()]);
            let _ = self.mpv.set("pause", "no");
            fade.switched = true;
        }
        if elapsed > 0.25 {
            self.fade = None;
            0.0
        } else if elapsed <= 0.105 {
            elapsed / 0.105
        } else {
            1.0 - (elapsed - 0.105) / 0.145
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
                self.mpv
                    .command(&["seek", &seconds.to_string(), "relative"])
            }
            Action::Seek(part) => {
                self.mpv
                    .command(&["seek", &(part * 100.0).to_string(), "absolute-percent"])
            }
            Action::Mute => {
                if self.mpv.get("volume").as_f64() == Some(0.0) {
                    self.mpv
                        .set("volume", &self.last_volume.to_string())
                        .and_then(|()| self.mpv.set("mute", "no"))
                } else {
                    self.mpv.command(&["cycle", "mute"])
                }
            }
            Action::Volume(delta) => {
                if self.menu.is_some() {
                    self.scroll = self
                        .scroll
                        .saturating_add_signed(-(delta.signum() as isize));
                    return;
                }
                self.mpv.command(&["add", "volume", &delta.to_string()])
            }
            Action::SetVolume(percent) => self
                .mpv
                .set("volume", &percent.to_string())
                .and_then(|()| self.mpv.set("mute", "no"))
                .map(|()| {
                    if percent > 0 {
                        self.last_volume = f64::from(percent);
                    }
                }),
            Action::ToggleSubtitles => self.mpv.command(&["cycle", "sub-visibility"]),
            Action::Audio(id) => self.mpv.set("aid", &id.to_string()),
            Action::Subtitles(id) => self
                .mpv
                .set("sid", &if id == 0 { "no".into() } else { id.to_string() }),
            Action::Speed(speed) => self.mpv.set("speed", speed),
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
                self.mpv.command(&["cycle", "fullscreen"])
            }
            Action::Minimize => self.mpv.set("window-minimized", "yes"),
            Action::Maximize => self.mpv.command(&["cycle", "window-maximized"]),
            Action::Quit => self.mpv.command(&["quit"]),
            Action::Clear => {
                let _ = self.mpv.command(&["playlist-clear"]);
                self.mpv.command(&["stop"])
            }
            Action::Jump(index) => {
                self.jump(index);
                return;
            }
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
            Action::ClickStage if self.media.kind == Kind::Video => Some(ClickArea::Video),
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

    fn pointer(&mut self, event: PointerEvent) {
        self.hovered = Instant::now();
        let mouse = mouse_position(&self.mpv);
        let target = mouse.map(|point| {
            self.frame
                .as_ref()
                .map_or(Action::ClickStage, |f| f.at(point))
        });
        match event {
            PointerEvent::Down => match target {
                Some(action @ Action::SetVolume(_)) => {
                    self.pending_click = None;
                    self.pressed = Some(action.clone());
                    self.action(action);
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
            },
            PointerEvent::Up => {
                self.drag_origin = None;
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
            }
        }
    }

    fn handle_event(&mut self, event: Event, metadata: &mut Instant) -> bool {
        match event {
            Event::Shutdown => return false,
            Event::Start(id) => {
                if self.live.as_ref().is_some_and(|live| live.clip_id != id) {
                    self.finish_live(false);
                }
                self.pending_click = None;
                self.refresh_media();
                *metadata = Instant::now();
                self.hovered = Instant::now();
            }
            Event::Loaded => {
                self.pending_click = None;
                self.refresh_media();
                if self.live.is_none() {
                    self.photos = (self.media.kind == Kind::Image)
                        .then(|| PhotoFolder::scan(&self.media.path))
                        .flatten();
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
                    self.note(format!("Couldn’t play this item: {error}"));
                }
                self.refresh_media();
            }
            Event::Pointer(event) => self.pointer(event),
            Event::PlaylistChanged => self.playlist_changed = Some(Instant::now()),
            Event::StreamText(text) => {
                if let Some(input) = &mut self.stream {
                    input.insert(&text);
                }
            }
            Event::Action(name) => {
                self.pending_click = None;
                if name.starts_with("stream-") {
                    self.stream_key(&name);
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
            let interval = if self.controls.active(Instant::now()) || self.dragging_volume() {
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
            if last_metadata.elapsed() > Duration::from_millis(250) {
                self.refresh_media();
                last_metadata = Instant::now();
            }
            let timing = Timings::read(&self.mpv);
            if self.live.is_some() && self.mpv.get("eof-reached").as_bool() == Some(true) {
                self.finish_live(true);
            }
            if timing.volume > 0.0 {
                self.last_volume = timing.volume;
            }
            if timing.mouse.is_some() && timing.mouse != self.timing.mouse {
                self.hovered = Instant::now();
            }
            self.drag_volume_to(timing.mouse);
            self.maybe_drag_window(timing.mouse);
            let (w, h) = timing.dimensions;
            let resize = timing.dimensions != self.timing.dimensions;
            self.timing = timing;
            let fade = self.fade_opacity();
            if w <= 0 || h <= 0 {
                continue;
            }
            #[cfg(windows)]
            if (resize || dark_hwnd.is_none())
                && let Some(hwnd) = self.mpv.get("window-id").as_i64().filter(|id| *id != 0)
            {
                platform::dark_windows_frame(hwnd);
                dark_hwnd = Some(hwnd);
            }
            let show_controls = self.menu.is_some()
                || self.stream.is_some()
                || self.media.kind != Kind::Video
                || self.timing.paused
                || self.dragging_volume()
                || self.hovered.elapsed() < Duration::from_secs(3);
            let controls_opacity = self.controls.update(show_controls, Instant::now());
            let frame = ui::draw(View {
                w,
                h,
                mouse: self.timing.mouse.unwrap_or((-100, -100)),
                media: &self.media,
                photo_position: self
                    .photos
                    .as_ref()
                    .filter(|_| self.media.kind == Kind::Image)
                    .map(|folder| (folder.index, folder.len())),
                live_available: self.media.kind == Kind::Image
                    && self
                        .photos
                        .as_ref()
                        .is_some_and(|folder| folder.live_clip.is_some()),
                live_playing: self.live.is_some(),
                stream: self.stream.as_ref().map(|input| ui::StreamView {
                    before: &input.text[..input.cursor],
                    after: &input.text[input.cursor..],
                    all: input.all,
                    invalid: input.invalid,
                }),
                paused: self.timing.paused,
                muted: self.timing.muted,
                fullscreen: self.timing.fullscreen,
                native_titlebar: (wayland || cfg!(target_os = "macos")) && self.timing.border,
                resize_grips: wayland
                    && !self.timing.border
                    && !self.timing.fullscreen
                    && !self.timing.maximized,
                controls_interactive: show_controls,
                controls_opacity,
                buffering: self.timing.buffering,
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
                speed: self.timing.speed,
                quality: self.quality,
                fade,
                toast: if self.toast.1.elapsed() < Duration::from_secs(4) {
                    &self.toast.0
                } else {
                    ""
                },
            });
            if resize
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

fn smoke(path: &str) -> Result<(), String> {
    let mpv = Engine::create(None, true, None)?;
    let path = library::normalize(path).ok_or("Not a playable path")?;
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
        if loaded {
            let media = Media::read(&mpv);
            if media.kind == Kind::Video
                && media.source_range == "Not reported"
                && start.elapsed() < Duration::from_millis(1200)
            {
                continue;
            }
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
    Err("Timed out opening the test media".into())
}

fn entry() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();
    if first.as_deref().is_some_and(|x| x == "--smoke-test") {
        return smoke(
            &arguments
                .next()
                .ok_or("Provide a file for --smoke-test")?
                .to_string_lossy(),
        );
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
        pending_click: None,
        last_volume: 100.0,
        picker: false,
        recent,
        fade: None,
        photos: None,
        live: None,
        stream: None,
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
