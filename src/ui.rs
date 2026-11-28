use crate::{
    fonts::Catalog,
    gpu::{DecodeChoice, DecoderGpu},
    prepare::{Method, Status},
    scale::{PERCENT, pixels},
    state::{Hdr, Kind, Media, PhotoZoom, RenderMode, SpeedStep, display_name, is_stream, time},
    subtitles::{CaptionStyle, Direction, ENCODINGS, Encoding},
};
use std::fmt::Write;

include!(concat!(env!("OUT_DIR"), "/osd_paths.rs"));

const BAR: &str = "171412";
const PANEL: &str = "1D1814";
const LINE: &str = "44352C";
const TEXT: &str = "F6EEE7";
const MUTED: &str = "BDB0A6";
const ACCENT: &str = "EDAA7B";
const OPEN_LEFT: i32 = 10;
const OPEN_WIDTH: i32 = 85;
const PLAYBACK_WIDTH: i32 = 312;
const SEEK_PREVIEW_WIDTH: i32 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Open,
    Slideshow,
    Prepare,
    Context,
    Playlist,
    Audio,
    Subtitles,
    SubtitleEncoding,
    SubtitleFont,
    Playback,
    Decoder,
    Rendering,
    Info,
    About,
    Shortcuts,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Menu(Menu),
    ClickStage,
    ClickBar,
    DragWindow,
    OpenFiles,
    AddFiles,
    OpenStream,
    SubmitStream,
    CancelStream,
    OpenSubtitles,
    OpenContainingFolder,
    ShowInPlaylist,
    Save,
    Load,
    Clear,
    Jump(usize),
    Remove(usize),
    Scroll(i32),
    Play,
    Previous,
    Next,
    LivePhoto,
    StartPreparation,
    CancelPreparation,
    PlayPrepared,
    PlayOriginal,
    PhotoZoom(PhotoZoom),
    SlideshowMusic,
    ClearSlideshowMusic,
    StartSlideshow,
    StopSlideshow,
    Skip(i32),
    Seek(f64),
    RepeatOne,
    Volume(i32),
    SetVolume(u8),
    Mute,
    ToggleSubtitles,
    Audio(i64),
    Subtitles(i64),
    SubtitleDirection(Direction),
    SubtitleEncoding(Encoding),
    SubtitleFont(Option<usize>),
    Speed(SpeedStep),
    Quality(&'static str),
    Hdr(Hdr),
    Decode(DecodeChoice),
    Render(RenderMode),
    Fullscreen,
    Minimize,
    Maximize,
    Quit,
    Escape,
    Link(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    fn contains(self, (x, y): (i32, i32)) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

// Rendering and hit testing share the same viewport transform.
struct Layout {
    physical: (i32, i32),
    logical: (i32, i32),
    backing_scale: f64,
}

impl Layout {
    fn new(w: i32, h: i32, backing_scale: f64) -> Self {
        let logical = |n: i32| {
            (f64::from(n) * 100.0 / (f64::from(PERCENT) * backing_scale))
                .round()
                .max(1.0) as i32
        };
        Self {
            physical: (w, h),
            logical: (logical(w), logical(h)),
            backing_scale,
        }
    }
    fn logical_point(&self, (x, y): (i32, i32)) -> (f64, f64) {
        (
            x as f64 * self.logical.0 as f64 / self.physical.0 as f64,
            y as f64 * self.logical.1 as f64 / self.physical.1 as f64,
        )
    }
    fn physical_rect(&self, r: Rect) -> (i32, i32, i32, i32) {
        let x = |x: i32| {
            (f64::from(x) * f64::from(self.physical.0) / f64::from(self.logical.0)).round() as i32
        };
        let y = |y: i32| {
            (f64::from(y) * f64::from(self.physical.1) / f64::from(self.logical.1)).round() as i32
        };
        (x(r.x), y(r.y), x(r.x + r.w) - x(r.x), y(r.y + r.h) - y(r.y))
    }
    fn pixel_aligned_icon(&self, (x, y): (i32, i32), size: i32) -> ((f64, f64), (f64, f64)) {
        let physical_size = (f64::from(pixels(size)) * self.backing_scale).round();
        let axis = |center: i32, logical: i32, physical: i32| {
            let scale = f64::from(physical) / f64::from(logical);
            let origin = (f64::from(center) * scale - physical_size / 2.0).round();
            (
                origin / scale,
                100.0 * physical_size / (f64::from(size) * scale),
            )
        };
        let (x, sx) = axis(x, self.logical.0, self.physical.0);
        let (y, sy) = axis(y, self.logical.1, self.physical.1);
        ((x, y), (sx, sy))
    }
    #[cfg(test)]
    fn physical_point(&self, (x, y): (i32, i32)) -> (i32, i32) {
        (
            (x as f64 * self.physical.0 as f64 / self.logical.0 as f64).round() as i32,
            (y as f64 * self.physical.1 as f64 / self.logical.1 as f64).round() as i32,
        )
    }
}

fn floor((x, y): (f64, f64)) -> (i32, i32) {
    (x.floor() as i32, y.floor() as i32)
}

struct VolumeSlider {
    hit: Rect,
    left: i32,
    width: i32,
}

impl VolumeSlider {
    fn percent(&self, x: f64) -> u8 {
        ((x - self.left as f64) / self.width as f64 * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    }
}

pub struct Frame {
    ass: String,
    layout: Layout,
    hits: Vec<(Rect, Action)>,
    seek: Option<Rect>,
    volume: Option<VolumeSlider>,
    opacity: u8,
    interactive: bool,
    panel_style: bool,
}

impl Frame {
    fn new(layout: Layout) -> Self {
        Self {
            ass: String::new(),
            layout,
            hits: vec![],
            seek: None,
            volume: None,
            opacity: 255,
            interactive: true,
            panel_style: false,
        }
    }
    pub fn osd(&self) -> &str {
        &self.ass
    }
    pub fn resolution(&self) -> (i32, i32) {
        self.layout.logical
    }
    pub fn volume_at(&self, point: (i32, i32), dragging: bool) -> Option<u8> {
        let logical = self.layout.logical_point(point);
        self.volume
            .as_ref()
            .filter(|slider| dragging || slider.hit.contains(floor(logical)))
            .map(|slider| slider.percent(logical.0))
    }
    pub fn at(&self, point: (i32, i32)) -> Action {
        if let Some(value) = self.volume_at(point, false) {
            return Action::SetVolume(value);
        }
        let logical = self.layout.logical_point(point);
        let point = floor(logical);
        if let Some(r) = self.seek.filter(|r| r.contains(point)) {
            return Action::Seek(((logical.0 - r.x as f64) / r.w as f64).clamp(0.0, 1.0));
        }
        self.hits
            .iter()
            .rev()
            .find_map(|(r, action)| r.contains(point).then_some(action))
            .cloned()
            .unwrap_or(Action::ClickStage)
    }
    fn transparency(&self, transparency: u8) -> u8 {
        255 - (((255 - u16::from(transparency)) * u16::from(self.opacity) + 127) / 255) as u8
    }
    fn path(&mut self, path: &str, color: &str, transparency: u8) {
        self.drawing((0., 0.), 1, (100., 100.), path, color, transparency);
    }
    fn drawing(
        &mut self,
        (x, y): (f64, f64),
        scale: i32,
        (sx, sy): (f64, f64),
        path: &str,
        color: &str,
        transparency: u8,
    ) {
        let transparency = self.transparency(transparency);
        let _ = writeln!(
            self.ass,
            "{{\\an7\\pos({x:.3},{y:.3})\\fscx{sx:.3}\\fscy{sy:.3}\\bord0\\shad0\\1c&H{}&\\1a&H{:02X}&\\p{scale}}}{path}{{\\p0}}",
            bgr(color),
            transparency
        );
    }
    fn rect(&mut self, r: Rect, radius: i32, color: &str, transparency: u8) {
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let (x, y, u, v) = (r.x, r.y, r.x + r.w, r.y + r.h);
        let d = radius.clamp(0, (r.w.min(r.h)) / 2);
        self.path(&format!(
            "m {} {y} l {} {y} b {u} {y} {u} {y} {u} {} l {u} {} b {u} {v} {u} {v} {} {v} l {} {v} b {x} {v} {x} {v} {x} {} l {x} {} b {x} {y} {x} {y} {} {y}",
            x+d,u-d,y+d,v-d,u-d,x+d,v-d,y+d,x+d), color, transparency);
    }
    fn text(
        &mut self,
        x: i32,
        y: i32,
        size: i32,
        align: i32,
        color: &str,
        bold: bool,
        value: &str,
    ) {
        let safe = value
            .replace('\\', "＼")
            .replace('{', "｛")
            .replace('}', "｝")
            .replace(['\n', '\r'], " ");
        let transparency = self.transparency(0);
        let _ = writeln!(
            self.ass,
            "{{\\an{align}\\pos({x},{y})\\fnDejaVu Sans\\fs{size}\\b{}\\bord0\\shad0\\1c&H{}&\\1a&H{transparency:02X}&}}{safe}",
            i32::from(bold),
            bgr(color)
        );
    }
    fn hit(&mut self, r: Rect, action: Action) {
        if self.interactive {
            self.hits.push((r, action));
        }
    }

    fn button(
        &mut self,
        r: Rect,
        label: &str,
        state: View<'_>,
        opts: (bool, bool),
        action: Action,
    ) {
        self.labeled_button(r, label, (None, None), state, opts, action);
    }

    fn labeled_button(
        &mut self,
        r: Rect,
        label: &str,
        icons: (Option<&'static str>, Option<&'static str>),
        state: View<'_>,
        opts: (bool, bool),
        action: Action,
    ) {
        let (primary, enabled) = opts;
        let hover = r.contains(state.mouse) && enabled;
        let (bg, fg) = match (primary, enabled, hover) {
            (true, _, true) => ("F3BA94", "211610"),
            (true, _, false) => (ACCENT, "211610"),
            (_, false, _) => ("211D1A", "918780"),
            (_, true, true) => ("402D22", TEXT),
            _ => ("2A211B", TEXT),
        };
        self.rect(r, 6, LINE, 0);
        self.rect(Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2), 5, bg, 0);
        let cy = r.y + r.h / 2;
        let mut cx = r.x + r.w / 2;
        let icon_size = if self.panel_style { pixels(14) } else { 14 };
        if let Some(name) = icons.0 {
            self.icon(r.x + 14, cy, icon_size, name, fg);
            cx += 9;
        }
        if let Some(name) = icons.1 {
            self.icon(r.x + r.w - 13, cy, icon_size, name, fg);
            cx -= 9;
        }
        self.text(
            cx,
            cy,
            if self.panel_style { pixels(12) } else { 12 },
            5,
            fg,
            true,
            label,
        );
        if enabled {
            self.hit(r, action);
        }
    }

    fn icon_button(
        &mut self,
        r: Rect,
        icon: &'static str,
        state: View<'_>,
        active: bool,
        enabled: bool,
        action: Action,
    ) {
        if enabled && (active || r.contains(state.mouse)) {
            self.rect(r, 6, "3C2B21", 0);
        }
        let col = if !enabled {
            "766960"
        } else if active {
            ACCENT
        } else {
            TEXT
        };
        let size = if matches!(icon, "Play" | "Pause") {
            20
        } else if self.panel_style {
            pixels(14)
        } else {
            19
        };
        self.icon(r.x + r.w / 2, r.y + r.h / 2, size, icon, col);
        if enabled {
            self.hit(r, action);
        }
    }

    fn icon(&mut self, x: i32, y: i32, size: i32, name: &'static str, color: &str) {
        let (origin, zoom) = self.layout.pixel_aligned_icon((x, y), size);
        self.drawing(origin, 6, zoom, icon_path(name, size), color, 0);
    }
}

fn bgr(hex: &str) -> String {
    format!("{}{}{}", &hex[4..6], &hex[2..4], &hex[0..2])
}

fn shorten(text: &str, length: usize) -> String {
    if text.chars().count() <= length {
        return text.to_owned();
    }
    text.chars()
        .take(length.saturating_sub(1))
        .chain(['…'])
        .collect()
}

#[derive(Clone, Copy)]
pub struct StreamView<'a> {
    pub before: &'a str,
    pub after: &'a str,
    pub all: bool,
    pub invalid: bool,
}

#[derive(Clone, Copy)]
pub struct PrepareView<'a> {
    pub cached: bool,
    pub legacy: bool,
    pub playing: bool,
    pub file_name: &'a str,
    pub status: Option<&'a Status>,
}

impl PrepareView<'_> {
    fn ready(self) -> bool {
        self.cached || matches!(self.status, Some(Status::Ready))
    }
}

#[derive(Clone, Copy)]
pub struct SeekHover {
    pub fraction: f64,
    box_rect: Rect,
    image: Rect,
    pub image_bounds: (i32, i32, i32, i32),
}

#[derive(Clone, Copy)]
pub struct TimelineView {
    pub hover: SeekHover,
    pub seconds: f64,
    pub has_image: bool,
}

#[derive(Clone, Copy)]
pub struct View<'a> {
    pub w: i32,
    pub h: i32,
    pub backing_scale: f64,
    pub mouse: (i32, i32),
    pub media: &'a Media,
    pub photo_position: Option<(usize, usize)>,
    pub live_available: bool,
    pub live_playing: bool,
    pub prepare: Option<PrepareView<'a>>,
    pub timeline: Option<TimelineView>,
    pub caption: Option<&'a str>,
    pub caption_style: CaptionStyle,
    pub subtitle_direction: Option<Direction>,
    pub subtitle_encoding: Encoding,
    pub subtitle_font: Option<&'a str>,
    pub font_catalog: &'a Catalog,
    pub font_query: &'a str,
    pub photo_zoom: PhotoZoom,
    pub slideshow_playing: bool,
    pub slideshow_music: Option<&'a str>,
    pub picking_music: bool,
    pub stream: Option<StreamView<'a>>,
    pub paused: bool,
    pub muted: bool,
    pub repeat_one: bool,
    pub fullscreen: bool,
    pub native_titlebar: bool,
    pub resize_grips: bool,
    pub controls_interactive: bool,
    pub controls_opacity: u8,
    pub buffering: bool,
    pub volume: f64,
    pub position: f64,
    pub duration: f64,
    pub buffered: f64,
    pub menu: Option<Menu>,
    pub context_anchor: Option<(i32, i32)>,
    pub scroll: usize,
    pub hdr: Hdr,
    pub devices: &'a [DecoderGpu],
    pub decoder: DecodeChoice,
    pub render_mode: RenderMode,
    pub fast_render: bool,
    pub speed: f64,
    pub seekable: bool,
    pub quality: &'a str,
    pub toast: &'a str,
}

const COMPACT_PHOTO_WIDTH: i32 = 320;

fn bottom_height(view: View<'_>) -> i32 {
    if view.media.kind == Kind::Image {
        if view.w < COMPACT_PHOTO_WIDTH { 77 } else { 44 }
    } else {
        video_bar_height(view.w)
    }
}

fn video_bar_height(w: i32) -> i32 {
    if w < 670 { 91 } else { 62 }
}

fn seek_track(w: i32, bar_y: i32) -> Rect {
    Rect::new(13, bar_y + 2, w - 26, 17)
}

pub fn seek_hover((w, h): (i32, i32), backing_scale: f64, point: (i32, i32)) -> Option<SeekHover> {
    let layout = Layout::new(w, h, backing_scale);
    let (w, h) = layout.logical;
    if w < 120 || h < 120 {
        return None;
    }
    let bar_y = h - video_bar_height(w);
    let track = seek_track(w, bar_y);
    let logical = layout.logical_point(point);
    if !track.contains(floor(logical)) || bar_y < 62 {
        return None;
    }
    let label_y = bar_y - 30;
    let width = (w - 16)
        .min(SEEK_PREVIEW_WIDTH)
        .min(((label_y - 8) * 16 / 9) + 8);
    let left = (logical.0.round() as i32 - width / 2).clamp(8, w - width - 8);
    let image_height = (width - 8) * 9 / 16;
    let image = Rect::new(left + 4, label_y - image_height, width - 8, image_height);
    Some(SeekHover {
        fraction: ((logical.0 - f64::from(track.x)) / f64::from(track.w)).clamp(0.0, 1.0),
        box_rect: Rect::new(left, image.y - 4, width, image_height + 26),
        image,
        image_bounds: layout.physical_rect(image),
    })
}

fn timeline_preview(f: &mut Frame, t: TimelineView) {
    let outer = t.hover.box_rect;
    let rect = if t.has_image {
        outer
    } else {
        let wide = outer.w.min(74);
        Rect::new(
            outer.x + (outer.w - wide) / 2,
            outer.y + outer.h - 24,
            wide,
            24,
        )
    };
    f.rect(rect, 6, LINE, 0);
    f.rect(
        Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2),
        5,
        PANEL,
        0,
    );
    let label_y = if t.has_image {
        t.hover.image.y + t.hover.image.h + 11
    } else {
        rect.y + 12
    };
    f.text(
        rect.x + rect.w / 2,
        label_y,
        12,
        5,
        TEXT,
        true,
        &time(t.seconds),
    );
}

fn subtitle_lift(w: i32, controls: bool) -> i32 {
    if controls { video_bar_height(w) + 6 } else { 0 }
}

pub fn subtitle_position((w, h): (i32, i32), backing_scale: f64, controls: bool) -> u8 {
    let (w, h) = Layout::new(w, h, backing_scale).logical;
    ((h - subtitle_lift(w, controls)).max(0) * 100 / h) as u8
}

pub fn draw(mut v: View<'_>) -> Frame {
    let mut f = Frame::new(Layout::new(v.w, v.h, v.backing_scale));
    (v.w, v.h) = f.resolution();
    v.mouse = floor(f.layout.logical_point(v.mouse));
    v.context_anchor = v
        .context_anchor
        .map(|point| floor(f.layout.logical_point(point)));
    let (w, h) = (v.w, v.h);
    if w < 120 || h < 120 {
        return f;
    }
    if v.media.kind == Kind::Video
        && let Some(text) = v.caption
    {
        let x = w / 2;
        let scale = f64::from(h) / 720.0;
        let style = v.caption_style;
        let y =
            (h - subtitle_lift(w, v.controls_opacity > 0) - (style.margin * scale).round() as i32)
                .max(h / 4);
        let size = style.size * scale;
        let outline = style.outline * scale;
        let _ = writeln!(
            f.ass,
            "{{\\an2\\pos({x},{y})\\fe-1\\fs{size:.2}\\bord{outline:.2}\\shad0\\1c&HFFFFFF&\\3c&H000000&}}{text}",
        );
    }
    let empty = v.media.kind == Kind::Empty;
    if empty {
        f.rect(Rect::new(0, 0, w, h), 0, "0C0B0A", 0);
        let center = h / 2 - pixels(24);
        f.text(
            w / 2,
            center - pixels(21),
            pixels(19),
            8,
            TEXT,
            true,
            "No media open",
        );
        f.text(
            w / 2,
            center + pixels(6),
            pixels(13),
            8,
            MUTED,
            false,
            "Drop videos, music, or images here to get started.",
        );
        let gap = pixels(7);
        let width = ((w - 25) / 3).clamp(66, pixels(120));
        let left = (w - width * 3 - 2 * gap) / 2;
        f.panel_style = true;
        for (i, label, icon, opts, action) in [
            (
                0,
                "Open Files",
                "FolderOpen",
                (true, true),
                Action::OpenFiles,
            ),
            (
                1,
                "Open Streams",
                "Link2",
                (false, true),
                Action::OpenStream,
            ),
            (2, "Open IPTV", "Tv", (false, false), Action::ClickStage),
        ] {
            f.labeled_button(
                Rect::new(
                    left + i * (width + gap),
                    center + pixels(35),
                    width,
                    pixels(37),
                ),
                label,
                ((width >= 105).then_some(icon), None),
                v,
                opts,
                action,
            );
        }
        f.panel_style = false;
    } else if v.media.kind == Kind::Music {
        let y = if v.media.cover {
            (h - bottom_height(v) - pixels(100)).max(63)
        } else {
            h / 2 - pixels(53)
        };
        let music_w = (w - pixels(36)).clamp(100, pixels(430));
        let box_rect = Rect::new(w / 2 - music_w / 2, y, music_w, pixels(92));
        f.rect(box_rect, pixels(12), LINE, 0);
        f.rect(
            Rect::new(box_rect.x + 1, y + 1, music_w - 2, box_rect.h - 2),
            pixels(11),
            PANEL,
            if v.media.cover { 26 } else { 0 },
        );
        f.icon(
            w / 2 - pixels(39),
            y + pixels(15),
            pixels(14),
            "Music2",
            ACCENT,
        );
        f.text(
            w / 2 + pixels(8),
            y + pixels(10),
            pixels(10),
            8,
            ACCENT,
            true,
            "NOW PLAYING",
        );
        f.text(
            w / 2,
            y + pixels(28),
            pixels(20),
            8,
            TEXT,
            true,
            &shorten(&v.media.track, (music_w / pixels(11)) as usize),
        );
        let tags = format!("{}   ·   {}", v.media.artist, v.media.album);
        f.text(
            w / 2,
            y + pixels(62),
            pixels(12),
            8,
            MUTED,
            false,
            &shorten(&tags, (music_w / pixels(7)) as usize),
        );
    }
    if v.controls_opacity > 0 {
        f.opacity = v.controls_opacity;
        f.interactive = v.controls_interactive;
        top(&mut f, v);
        bottom(&mut f, v);
        if let Some(timeline) = v.timeline {
            timeline_preview(&mut f, timeline);
        }
        if v.resize_grips {
            resize_grips(&mut f, w, h);
        }
        if v.slideshow_playing {
            photo_actions(&mut f, v);
        }
        if let Some(prepare) = v.prepare {
            let rect = prepare_anchor(v);
            let busy = matches!(prepare.status, Some(Status::Running(..)));
            f.labeled_button(
                rect,
                &shorten("Smooth Playback", ((rect.w - 10) / 6).max(8) as usize),
                (None, (rect.w >= 145).then_some("ChevronDown")),
                v,
                (v.menu == Some(Menu::Prepare) || busy, true),
                Action::Menu(Menu::Prepare),
            );
        }
        f.opacity = 255;
        f.interactive = true;
    }
    if !v.slideshow_playing {
        photo_actions(&mut f, v);
    }
    if v.buffering && !empty && v.menu.is_none() {
        let status = if is_stream(&v.media.path) {
            "Buffering stream…"
        } else {
            "Waiting for the file or decoder…"
        };
        let width = pixels(238).min(w - 12);
        f.rect(
            Rect::new((w - width) / 2, h / 2 - pixels(21), width, pixels(42)),
            pixels(9),
            PANEL,
            16,
        );
        f.text(
            w / 2,
            h / 2,
            pixels(13),
            5,
            TEXT,
            false,
            &shorten(status, (width / 7) as usize),
        );
    }
    if let Some(menu) = v.menu {
        f.panel_style = true;
        if menu == Menu::Prepare {
            if let Some(prepare) = v.prepare {
                prepare_panel(&mut f, v, prepare);
            }
        } else {
            menu_panel(&mut f, v, menu);
        }
        f.panel_style = false;
    }
    if !v.toast.is_empty() {
        let wide = pixels(v.toast.chars().count().min(300) as i32 * 7 + 24)
            .clamp(100, (w - pixels(24)).max(100));
        let r = Rect::new(
            w - wide - pixels(12),
            h - bottom_height(v) - pixels(47),
            wide,
            pixels(35),
        );
        f.rect(r, pixels(7), "32231B", 9);
        f.text(
            r.x + pixels(11),
            r.y + pixels(8),
            pixels(13),
            7,
            TEXT,
            false,
            &shorten(v.toast, (wide / pixels(7)).saturating_sub(3) as usize),
        );
    }
    if let Some(stream) = v.stream {
        stream_panel(&mut f, v, stream);
    }
    f
}

fn photo_actions(f: &mut Frame, v: View<'_>) {
    if (v.live_available && !v.slideshow_playing) || v.live_playing {
        f.labeled_button(
            Rect::new(v.w - 102, 57, 89, 32),
            if v.live_playing { "Stop" } else { "LIVE" },
            (Some("LivePhoto"), None),
            v,
            (v.live_playing, true),
            Action::LivePhoto,
        );
    }
    if v.photo_position.is_some() || v.slideshow_playing {
        let rect = slideshow_anchor(v);
        let compact = rect.w < 106;
        f.labeled_button(
            rect,
            if v.slideshow_playing {
                if compact { "Stop" } else { "Stop slideshow" }
            } else {
                "Slideshow"
            },
            (
                (!compact).then_some(if v.slideshow_playing { "Pause" } else { "Play" }),
                (!compact && !v.slideshow_playing).then_some("ChevronDown"),
            ),
            v,
            (v.slideshow_playing || v.menu == Some(Menu::Slideshow), true),
            if v.slideshow_playing {
                Action::StopSlideshow
            } else {
                Action::Menu(Menu::Slideshow)
            },
        );
    }
}

fn slideshow_anchor(v: View<'_>) -> Rect {
    let beside_live = v.live_available && !v.slideshow_playing && v.w >= 234;
    let edge = if beside_live { v.w - 110 } else { v.w - 13 };
    let y = if v.live_available && !v.slideshow_playing && !beside_live {
        94
    } else {
        57
    };
    let width = (edge - 10).min(137);
    Rect::new(edge - width, y, width, 32)
}

fn prepare_anchor(v: View<'_>) -> Rect {
    let edge = v.w - 13;
    let width = (edge - 10).min(164);
    Rect::new(edge - width, 57, width, 32)
}

fn prepare_panel(f: &mut Frame, v: View<'_>, prepare: PrepareView<'_>) {
    let width = (v.w - 20).min(pixels(380));
    let x = v.w - width - 10;
    let running = match prepare.status {
        Some(Status::Running(method, progress)) => Some((*method, *progress)),
        _ => None,
    };
    let base = pixels(if running.is_some() { 103 } else { 77 });
    let panel_room = (v.h - bottom_height(v) - 51).max(base);
    let line_width = ((width - pixels(24)) / pixels(7)).max(8) as usize;
    let filename = &format!("MP4: {}", prepare.file_name);
    let (messages, action, label, primary): (&[&str], _, _, _) = match prepare.status {
        Some(Status::Running(method, _)) => (
            &[
                if *method == Method::Reuse {
                    "Saving your previous copy as an MP4."
                } else {
                    "Making a reusable 1080p SDR copy."
                },
                "Saving next to the original.",
                filename,
            ],
            Action::CancelPreparation,
            "Cancel",
            false,
        ),
        _ if prepare.playing => (
            &[
                "Playing the 1080p SDR copy.",
                "Audio and subtitles are still from the original.",
            ],
            Action::PlayOriginal,
            "Use original",
            false,
        ),
        _ if prepare.ready() => (
            &[
                "The 1080p SDR copy is ready.",
                filename,
                "The original audio and subtitles are kept.",
            ],
            Action::PlayPrepared,
            "Play prepared",
            true,
        ),
        Some(Status::Failed(error)) => (
            &["Couldn’t prepare this video.", error],
            Action::StartPreparation,
            "Retry",
            true,
        ),
        Some(Status::Cancelled) => (
            &["Preparation canceled. No copy was saved."],
            Action::StartPreparation,
            "Start",
            true,
        ),
        _ if prepare.legacy => (
            &[
                "Save your previous copy next to the original, without re-encoding.",
                filename,
            ],
            Action::StartPreparation,
            "Save copy",
            true,
        ),
        _ => (
            &[
                "Makes a reusable 1080p SDR copy for smoother playback.",
                "Saves the MP4 next to the original.",
                filename,
                "Uses the GPU when available and pauses this video while it works.",
                "Your original stays untouched.",
            ],
            Action::StartPreparation,
            "Start",
            true,
        ),
    };
    let line_height = pixels(15);
    let mut lines = Vec::new();
    for message in messages {
        let mut line = String::new();
        for word in message.split_whitespace() {
            if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > line_width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    let rows = ((panel_room - base) / line_height).clamp(0, 7) as usize;
    let clipped = lines.len() > rows;
    lines.truncate(rows);
    if clipped && let Some(last) = lines.last_mut() {
        last.push('…');
    }
    let height = base + lines.len() as i32 * line_height;
    let y = (prepare_anchor(v).y + 37).min((v.h - bottom_height(v) - height - 4).max(48));
    f.rect(Rect::new(x, y, width, height), pixels(9), LINE, 0);
    f.rect(
        Rect::new(x + 1, y + 1, width - 2, height - 2),
        pixels(8),
        PANEL,
        0,
    );
    f.hit(Rect::new(x, y, width, height), Action::Scroll(0));
    f.text(
        x + pixels(12),
        y + pixels(11),
        pixels(10),
        7,
        ACCENT,
        true,
        &shorten(
            "SMOOTH PLAYBACK",
            ((width - pixels(34)) / pixels(6)).max(7) as usize,
        ),
    );
    f.icon_button(
        Rect::new(x + width - pixels(25), y + 2, pixels(23), pixels(23)),
        "X",
        v,
        false,
        true,
        Action::Escape,
    );
    for (i, line) in lines.iter().enumerate() {
        f.text(
            x + pixels(12),
            y + pixels(31) + i as i32 * line_height,
            pixels(12),
            7,
            MUTED,
            false,
            &shorten(line, line_width),
        );
    }
    if let Some((method, progress)) = running {
        let left = x + pixels(12);
        let status_y = y + height - pixels(67);
        let bar_w = width - pixels(24);
        let fraction = progress.fraction().unwrap_or_default();
        let timing = if fraction >= 0.995 {
            "Finishing…".into()
        } else if let Some(eta) = progress.eta() {
            format!("ETA {}", time(eta))
        } else {
            "Estimating ETA…".into()
        };
        let status = format!(
            "{} · {}% · {timing}",
            match method {
                Method::Gpu => "GPU-assisted",
                Method::Cpu => "CPU",
                Method::Reuse => "Copying",
            },
            (fraction * 100.0).floor().min(99.0) as u8,
        );
        f.text(
            left,
            status_y,
            pixels(11),
            7,
            TEXT,
            true,
            &shorten(&status, line_width),
        );
        f.rect(
            Rect::new(left, status_y + pixels(16), bar_w, pixels(5)),
            2,
            LINE,
            0,
        );
        f.rect(
            Rect::new(
                left,
                status_y + pixels(16),
                (bar_w as f64 * fraction).round() as i32,
                pixels(5),
            ),
            2,
            ACCENT,
            0,
        );
    }
    let button_w = pixels(115).min(width - pixels(20));
    f.button(
        Rect::new(
            x + width - button_w - pixels(10),
            y + height - pixels(37),
            button_w,
            pixels(29),
        ),
        label,
        v,
        (primary, true),
        action,
    );
}

fn stream_panel(f: &mut Frame, v: View<'_>, stream: StreamView<'_>) {
    f.hits.clear();
    f.seek = None;
    f.volume = None;
    f.panel_style = true;
    f.rect(Rect::new(0, 0, v.w, v.h), 0, "0C0B0A", 105);
    f.hit(Rect::new(0, 0, v.w, v.h), Action::Scroll(0));
    let width = (v.w - 24).min(pixels(405));
    let height = pixels(151);
    let x = (v.w - width) / 2;
    let y = ((v.h - height) / 2).max(0);
    f.rect(Rect::new(x, y, width, height), 10, LINE, 0);
    f.rect(Rect::new(x + 1, y + 1, width - 2, height - 2), 9, PANEL, 0);
    f.text(x + 14, y + 12, pixels(14), 7, TEXT, true, "Open stream");
    f.text(
        x + 14,
        y + 37,
        pixels(11),
        7,
        if stream.invalid { "FF9D85" } else { MUTED },
        false,
        &shorten(
            if stream.invalid {
                "Enter a valid HTTP or HTTPS URL."
            } else {
                "HLS, DASH, or a direct HTTP(S) media URL."
            },
            ((width - 28) / 7).max(1) as usize,
        ),
    );
    let input = Rect::new(x + 13, y + 62, width - 26, 32);
    f.rect(input, 5, if stream.invalid { "C16851" } else { ACCENT }, 30);
    f.rect(
        Rect::new(input.x + 1, input.y + 1, input.w - 2, input.h - 2),
        4,
        if stream.all { "553628" } else { "14100E" },
        0,
    );
    let limit = ((input.w - 20) / 7).max(3) as usize;
    let mut suffix: Vec<_> = stream
        .before
        .chars()
        .rev()
        .take(limit.saturating_sub(1))
        .collect();
    suffix.reverse();
    let before: String = suffix.into_iter().collect();
    let room = limit.saturating_sub(before.chars().count() + 1);
    let after = if room == 0 {
        String::new()
    } else {
        shorten(stream.after, room)
    };
    let url = format!("{before}{}{after}", if stream.all { "" } else { "│" });
    f.text(input.x + 8, input.y + 8, pixels(12), 7, TEXT, false, &url);
    let button_w = ((width - 33) / 2).min(100);
    let button_y = y + height - 43;
    f.button(
        Rect::new(x + width - (button_w + 13) * 2 + 5, button_y, button_w, 30),
        "Cancel",
        v,
        (false, true),
        Action::CancelStream,
    );
    f.button(
        Rect::new(x + width - button_w - 13, button_y, button_w, 30),
        "Open stream",
        v,
        (true, true),
        Action::SubmitStream,
    );
}

fn resize_grips(f: &mut Frame, w: i32, h: i32) {
    let color = "6B594C";
    for (x, side) in [(2, 2), (w - 10, w - 3)] {
        for (y, top) in [(2, 2), (h - 10, h - 3)] {
            f.rect(Rect::new(x, top, 8, 1), 0, color, 35);
            f.rect(Rect::new(side, y, 1, 8), 0, color, 35);
        }
    }
}

fn top(f: &mut Frame, v: View<'_>) {
    f.rect(Rect::new(0, 0, v.w, 44), 0, BAR, 9);
    f.rect(Rect::new(0, 43, v.w, 1), 0, LINE, 0);
    f.hit(Rect::new(0, 0, v.w, 44), Action::DragWindow);
    f.labeled_button(
        Rect::new(OPEN_LEFT, 7, OPEN_WIDTH, 30),
        "Open",
        (Some("FolderOpen"), Some("ChevronDown")),
        v,
        (false, true),
        Action::Menu(Menu::Open),
    );
    let window_buttons = !v.fullscreen && !v.native_titlebar;
    let right = if window_buttons { 100 } else { 8 };
    if v.w > 370 {
        let tag = match v.media.kind {
            Kind::Empty => "NO MEDIA",
            Kind::Music => "AUDIO FILE",
            Kind::Image => "IMAGE",
            Kind::Video if is_stream(&v.media.path) => "STREAM",
            _ => "FILE",
        };
        let badge = Rect::new(
            OPEN_LEFT + OPEN_WIDTH + 12,
            12,
            tag.chars().count() as i32 * 7 + 10,
            20,
        );
        f.rect(badge, 4, "2C241E", 0);
        f.text(badge.x + badge.w / 2, badge.y + 10, 9, 5, MUTED, true, tag);
        let x = badge.x + badge.w + 10;
        f.text(
            x,
            15,
            12,
            7,
            TEXT,
            false,
            &shorten(&v.media.title, ((v.w - right - 76 - x) / 7).max(0) as usize),
        );
    }
    for (offset, icon, menu) in [(67, "Info", Menu::About), (34, "Keyboard", Menu::Shortcuts)] {
        if v.w - right - offset < OPEN_LEFT + OPEN_WIDTH + 6 {
            continue;
        }
        f.icon_button(
            Rect::new(v.w - right - offset, 7, 30, 30),
            icon,
            v,
            v.menu == Some(menu),
            true,
            Action::Menu(menu),
        );
    }
    if window_buttons {
        for (offset, icon, action) in [
            (93, "Minus", Action::Minimize),
            (63, "Square", Action::Maximize),
            (33, "X", Action::Quit),
        ] {
            f.icon_button(
                Rect::new(v.w - offset, 7, 30, 30),
                icon,
                v,
                false,
                true,
                action,
            );
        }
    }
}

fn bottom(f: &mut Frame, v: View<'_>) {
    let height = bottom_height(v);
    let y = v.h - height;
    let image = v.media.kind == Kind::Image;
    let none = v.media.kind == Kind::Empty;
    f.rect(Rect::new(0, y, v.w, height), 0, BAR, 9);
    f.rect(Rect::new(0, y, v.w, 1), 0, LINE, 0);
    f.hit(Rect::new(0, y, v.w, height), Action::ClickBar);
    let row = y + if image { 4 } else { 20 };
    let right_row = if v.w < 670 && !image { row + 32 } else { row };
    if !image {
        let slider = seek_track(v.w, y);
        let bar_w = slider.w;
        f.rect(Rect::new(13, y + 9, bar_w, 4), 2, "514944", 0);
        if v.duration > 0.0 {
            let portion = (v.position / v.duration).clamp(0.0, 1.0);
            let buffered = ((v.position + v.buffered) / v.duration).clamp(0.0, 1.0);
            f.rect(
                Rect::new(13, y + 9, (bar_w as f64 * buffered) as i32, 4),
                2,
                "856F61",
                0,
            );
            let progress = (bar_w as f64 * portion) as i32;
            f.rect(Rect::new(13, y + 9, progress, 4), 2, ACCENT, 0);
            f.rect(Rect::new(10 + progress, y + 6, 9, 9), 4, "F5B78D", 0);
            if f.interactive && v.menu.is_none() {
                f.seek = Some(slider);
            }
        }
        f.icon_button(
            Rect::new(9, row, 34, 32),
            if v.paused || none { "Play" } else { "Pause" },
            v,
            !none,
            !none,
            Action::Play,
        );
    }
    let x = if image { 9 } else { 44 };
    let (index, total) = v
        .photo_position
        .unwrap_or((v.media.index.unwrap_or(0), v.media.playlist.len()));
    f.icon_button(
        Rect::new(x, row, 32, 32),
        "SkipBack",
        v,
        false,
        !none && index > 0,
        Action::Previous,
    );
    f.icon_button(
        Rect::new(x + 33, row, 32, 32),
        "SkipForward",
        v,
        false,
        !none && index + 1 < total,
        Action::Next,
    );
    if image {
        let status = format!("Image {} of {}", index + 1, total.max(1));
        let reserve = if v.w < COMPACT_PHOTO_WIDTH { 110 } else { 239 };
        let space = v.w - reserve - x - 72;
        if space >= 42 {
            f.text(
                x + 72,
                row + 10,
                12,
                7,
                MUTED,
                false,
                &shorten(&status, (space / 7) as usize),
            );
        }
    } else {
        let repeat_left = if v.w >= 290 {
            f.icon_button(
                Rect::new(x + 66, row, 33, 32),
                "RotateCcw",
                v,
                false,
                !none,
                Action::Skip(-10),
            );
            f.icon_button(
                Rect::new(x + 100, row, 33, 32),
                "RotateCw",
                v,
                false,
                !none,
                Action::Skip(10),
            );
            x + 134
        } else {
            x + 66
        };
        f.icon_button(
            Rect::new(repeat_left, row, 34, 32),
            "Repeat1",
            v,
            v.repeat_one,
            !none && !v.live_playing,
            Action::RepeatOne,
        );
        let volume_left = repeat_left + 35;
        f.icon_button(
            Rect::new(volume_left, row, 35, 32),
            if v.muted || v.volume == 0.0 {
                "VolumeX"
            } else {
                "Volume2"
            },
            v,
            false,
            true,
            Action::Mute,
        );
        let left = volume_left + 41;
        let width = 58.min(v.w - left - 12);
        if width >= 24 {
            let volume = if v.muted {
                0.0
            } else {
                v.volume.clamp(0.0, 100.0)
            };
            let filled = (width as f64 * volume / 100.0).round() as i32;
            f.rect(Rect::new(left, row + 15, width, 3), 2, "514944", 0);
            f.rect(Rect::new(left, row + 15, filled, 3), 2, ACCENT, 0);
            f.rect(Rect::new(left + filled - 4, row + 12, 8, 8), 4, "F5B78D", 0);
            if f.interactive && v.menu.is_none() {
                f.volume = Some(VolumeSlider {
                    hit: Rect::new(left - 4, row + 3, width + 9, 26),
                    left,
                    width,
                });
            }
            let space = v.w - left - width - 14;
            let status = if space >= 85 {
                format!("{}  /  {}", time(v.position), time(v.duration))
            } else {
                time(v.position)
            };
            if space >= 36 {
                f.text(left + width + 10, row + 10, 11, 7, TEXT, false, &status);
            }
        }
    }
    let mut right = v.w - 7;
    for (icon, menu, action) in [
        (
            if v.fullscreen { "Minimize" } else { "Expand" },
            None,
            Action::Fullscreen,
        ),
        ("SquarePlay", Some(Menu::Info), Action::Menu(Menu::Info)),
        (
            "ListVideo",
            Some(Menu::Playlist),
            Action::Menu(Menu::Playlist),
        ),
        (
            "Settings",
            Some(Menu::Playback),
            Action::Menu(Menu::Playback),
        ),
        (
            "Captions",
            Some(Menu::Subtitles),
            Action::Menu(Menu::Subtitles),
        ),
        ("Headphones", Some(Menu::Audio), Action::Menu(Menu::Audio)),
    ] {
        if image && !matches!(icon, "Expand" | "Minimize" | "SquarePlay" | "ListVideo") {
            continue;
        }
        right -= 34;
        let r = Rect::new(right, right_row, 32, 32);
        f.icon_button(r, icon, v, menu.is_some() && v.menu == menu, true, action);
    }
    if image {
        let (x, y) = if v.w < COMPACT_PHOTO_WIDTH {
            (9, row + 34)
        } else {
            (right - 128, row)
        };
        photo_zoom(f, v, x, y);
    }
}

fn photo_zoom(f: &mut Frame, v: View<'_>, x: i32, y: i32) {
    f.rect(Rect::new(x, y, 120, 32), 6, LINE, 0);
    f.rect(Rect::new(x + 1, y + 1, 118, 30), 5, "211A16", 0);
    for (offset, width, label, zoom) in [
        (1, 36, "Fit", PhotoZoom::Fit),
        (37, 36, "Fill", PhotoZoom::Fill),
        (73, 46, "100%", PhotoZoom::Actual),
    ] {
        let part = Rect::new(x + offset, y + 1, width, 30);
        let active = v.photo_zoom == zoom;
        if active || part.contains(v.mouse) {
            f.rect(part, 4, if active { "3D2A1E" } else { "31251D" }, 0);
        }
        if offset > 1 {
            f.rect(Rect::new(part.x, y + 7, 1, 18), 0, LINE, 0);
        }
        f.text(
            part.x + width / 2,
            y + 16,
            12,
            5,
            if active { ACCENT } else { TEXT },
            active,
            label,
        );
        if v.menu.is_none() {
            f.hit(part, Action::PhotoZoom(zoom));
        }
    }
}

struct Row {
    label: String,
    action: Option<Action>,
    active: bool,
    remove: Option<usize>,
    speed_stepper: bool,
}
fn row(label: impl Into<String>, action: Action) -> Row {
    Row {
        label: label.into(),
        action: Some(action),
        active: false,
        remove: None,
        speed_stepper: false,
    }
}
fn note(label: impl Into<String>) -> Row {
    Row {
        label: label.into(),
        action: None,
        active: false,
        remove: None,
        speed_stepper: false,
    }
}
fn selected(label: impl Into<String>, action: Action, active: bool) -> Row {
    Row {
        label: label.into(),
        action: Some(action),
        active,
        remove: None,
        speed_stepper: false,
    }
}

fn speed_label(speed: f64) -> String {
    let magnitude = format!("{:.2}", speed.abs());
    let display = magnitude.strip_suffix('0').unwrap_or(&magnitude);
    format!("{}{display}×", if speed < 0.0 { "−" } else { "" })
}

fn speed_buttons(f: &mut Frame, v: View<'_>, row: Rect) {
    let width = row.w.min(pixels(158));
    let r = Rect::new(row.x + (row.w - width) / 2, row.y, width, row.h);
    f.rect(r, 6, LINE, 0);
    f.rect(
        Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2),
        5,
        "211A16",
        0,
    );
    let side = pixels(38).min(r.w / 4);
    for (step, icon, x, divider) in [
        (SpeedStep::Decrease, "Minus", r.x + 1, r.x + 1 + side),
        (
            SpeedStep::Increase,
            "Plus",
            r.x + r.w - side - 1,
            r.x + r.w - side - 2,
        ),
    ] {
        f.icon_button(
            Rect::new(x, r.y + 1, side, r.h - 2),
            icon,
            v,
            false,
            step.next(v.speed, v.seekable).is_some(),
            Action::Speed(step),
        );
        f.rect(Rect::new(divider, r.y + 5, 1, r.h - 10), 0, LINE, 0);
    }
    f.text(
        r.x + r.w / 2,
        r.y + r.h / 2,
        pixels(12),
        5,
        ACCENT,
        true,
        &speed_label(v.speed),
    );
}

fn menu_panel(f: &mut Frame, v: View<'_>, menu: Menu) {
    let width = match menu {
        Menu::Open => (v.w - OPEN_LEFT - 10).min(pixels(212)),
        Menu::Slideshow => (v.w - 20).min(pixels(247)),
        Menu::Context => (v.w - 16).min(pixels(200)),
        Menu::Playback => (v.w - pixels(20)).min(pixels(PLAYBACK_WIDTH)),
        _ => (v.w - pixels(20)).min(pixels(390)),
    };
    let mut rows = Vec::new();
    let title = match menu {
        Menu::Prepare => return,
        Menu::Open => {
            rows.extend([
                row("Open files…", Action::OpenFiles),
                row("Add files to queue", Action::AddFiles),
                row("Open a stream…", Action::OpenStream),
            ]);
            "OPEN"
        }
        Menu::Slideshow => {
            rows.push(note("5 seconds per photo · loops"));
            rows.push(note(v.slideshow_music.map_or_else(
                || "No music selected".to_owned(),
                |path| display_name(path),
            )));
            rows.push(if v.picking_music {
                note("Choosing music…")
            } else {
                row("Open music…", Action::SlideshowMusic)
            });
            if v.slideshow_music.is_some() && !v.picking_music {
                rows.push(row("Remove music", Action::ClearSlideshowMusic));
            }
            "SLIDESHOW"
        }
        Menu::Context => {
            let playback = if v.paused { "Play" } else { "Pause" };
            rows.push(if matches!(v.media.kind, Kind::Video | Kind::Music) {
                row(playback, Action::Play)
            } else {
                note("Play / Pause")
            });
            rows.push(if v.media.containing_folder().is_some() {
                row("Open containing folder", Action::OpenContainingFolder)
            } else {
                note("Open containing folder")
            });
            rows.push(if v.media.index.is_some() {
                row("Show in playlist", Action::ShowInPlaylist)
            } else {
                note("Show in playlist")
            });
            rows.push(row("Quit", Action::Quit));
            "PLAYBACK"
        }
        Menu::Playlist => {
            for (i, item) in v.media.playlist.iter().enumerate() {
                let mut r = selected(
                    format!("{:02}  {}", i + 1, display_name(item)),
                    Action::Jump(i),
                    v.media.index == Some(i),
                );
                r.remove = Some(i);
                rows.push(r);
            }
            if rows.is_empty() {
                rows.push(note("No items. Drop media or choose Add files."));
            }
            "PLAYLIST"
        }
        Menu::Audio | Menu::Subtitles => {
            let audio = menu == Menu::Audio;
            if !audio {
                rows.push(row("Off", Action::Subtitles(0)));
            }
            rows.extend(
                v.media
                    .tracks
                    .iter()
                    .filter(|t| t.kind == if audio { "audio" } else { "sub" })
                    .map(|t| {
                        selected(
                            &t.title,
                            if audio {
                                Action::Audio(t.id)
                            } else {
                                Action::Subtitles(t.id)
                            },
                            t.selected,
                        )
                    }),
            );
            if audio && rows.is_empty() {
                rows.push(note("No selectable audio tracks."));
            }
            if !audio {
                rows.push(row("Add a subtitle file…", Action::OpenSubtitles));
            }
            if audio { "AUDIO TRACKS" } else { "SUBTITLES" }
        }
        Menu::SubtitleEncoding => {
            rows.extend(ENCODINGS.iter().map(|&encoding| {
                selected(
                    encoding.label,
                    Action::SubtitleEncoding(encoding),
                    v.subtitle_encoding == encoding,
                )
            }));
            "SUBTITLE ENCODING"
        }
        Menu::SubtitleFont => {
            let query = v.font_query.trim().to_lowercase();
            if query.is_empty() {
                rows.push(selected(
                    "System default",
                    Action::SubtitleFont(None),
                    v.subtitle_font.is_none(),
                ));
            }
            match v.font_catalog {
                Catalog::Ready(fonts) => {
                    let start = rows.len();
                    rows.extend(fonts.iter().enumerate().filter_map(|(i, font)| {
                        font.to_lowercase().contains(&query).then(|| {
                            selected(
                                font,
                                Action::SubtitleFont(Some(i)),
                                v.subtitle_font == Some(font),
                            )
                        })
                    }));
                    if rows.len() == start {
                        rows.push(note(if query.is_empty() {
                            "No system fonts found."
                        } else {
                            "No matching fonts."
                        }));
                    }
                }
                Catalog::Failed => rows.push(note("Couldn’t read the system fonts.")),
                _ => rows.push(note("Loading system fonts…")),
            }
            "SUBTITLE FONT"
        }
        Menu::Playback => {
            if matches!(v.media.kind, Kind::Video | Kind::Music) {
                rows.push(note(if v.seekable {
                    "Speed · −64× reverse / +64× forward"
                } else {
                    "Speed · reverse needs a seekable file"
                }));
                rows.push(Row {
                    speed_stepper: true,
                    ..note("")
                });
            }
            if v.media.kind != Kind::Music {
                rows.extend([
                    note(format!(
                        "HDR · {} → {}",
                        v.media.source_range, v.media.output_range
                    )),
                    selected("Off (SDR only)", Action::Hdr(Hdr::Off), v.hdr == Hdr::Off),
                    selected("Automatic HDR", Action::Hdr(Hdr::Auto), v.hdr == Hdr::Auto),
                    selected(
                        "Prefer native HDR",
                        Action::Hdr(Hdr::Prefer),
                        v.hdr == Hdr::Prefer,
                    ),
                    row("Video decoder", Action::Menu(Menu::Decoder)),
                    row("Rendering quality", Action::Menu(Menu::Rendering)),
                ]);
            }
            if is_stream(&v.media.path) {
                rows.push(note("HLS bitrate cap · reloads the stream"));
                for (label, value) in [
                    ("Best available", "max"),
                    ("Up to 20 Mbps", "20000000"),
                    ("Up to 8 Mbps", "8000000"),
                    ("Lowest available", "min"),
                ] {
                    rows.push(selected(label, Action::Quality(value), v.quality == value));
                }
            }
            "PLAYBACK"
        }
        Menu::Decoder => {
            rows.push(selected(
                "Automatic (recommended)",
                Action::Decode(DecodeChoice::Auto),
                v.decoder == DecodeChoice::Auto,
            ));
            for (i, device) in v.devices.iter().enumerate() {
                let choice = DecodeChoice::Gpu(i);
                rows.push(selected(
                    &device.name,
                    Action::Decode(choice),
                    v.decoder == choice,
                ));
            }
            rows.push(selected(
                "Software only (CPU)",
                Action::Decode(DecodeChoice::Software),
                v.decoder == DecodeChoice::Software,
            ));
            if v.media.kind == Kind::Video {
                rows.push(note(format!("Active decoder: {}", v.media.decoder)));
            }
            if v.media.software_av1 {
                rows.push(note("AV1 is being decoded on the CPU."));
                rows.push(note("If playback stutters, try HEVC Main 10."));
            } else {
                rows.push(note("Unsupported formats fall back to the CPU."));
            }
            rows.push(row("Back to Playback", Action::Menu(Menu::Playback)));
            "VIDEO DECODER"
        }
        Menu::Rendering => {
            for (label, mode) in [
                ("Automatic (fast for 4K50+)", RenderMode::Auto),
                ("Quality", RenderMode::Quality),
                ("Fast", RenderMode::Fast),
            ] {
                rows.push(selected(label, Action::Render(mode), v.render_mode == mode));
            }
            rows.push(note(if v.fast_render {
                "Active: Fast · lighter scaling"
            } else {
                "Active: Quality · higher-quality scaling"
            }));
            rows.push(note("Fast uses static HDR tone mapping when needed."));
            rows.push(row("Back to Playback", Action::Menu(Menu::Playback)));
            "RENDERING QUALITY"
        }
        Menu::Info => {
            if v.media.kind == Kind::Empty {
                rows.push(note("Open media to see its technical details."));
            } else {
                rows.extend(
                    v.media
                        .infos
                        .iter()
                        .map(|(name, value)| note(format!("{name}   ·   {value}"))),
                );
            }
            match v.media.kind {
                Kind::Image => "IMAGE INFO",
                Kind::Music => "AUDIO INFO",
                _ => "VIDEO INFO",
            }
        }
        Menu::About => {
            rows.extend([
                note("Coffee"),
                note("Video, photo and music playback."),
                note("Built in Rust. Rendered by libmpv."),
                row("GitHub", Action::Link("https://github.com/arazgray/coffee")),
                row("Made by Araz Gray", Action::Link("https://arazgray.com")),
            ]);
            "ABOUT COFFEE"
        }
        Menu::Shortcuts => {
            for label in [
                "Space / K  —  Play or pause",
                "← / →  —  Seek or browse images",
                "[ / ]  —  Previous or next item",
                "↑ / ↓  —  Volume",
                "Enter / F  —  Fullscreen",
                if cfg!(target_os = "macos") {
                    "O / Cmd+O  —  Open files"
                } else {
                    "O  —  Open files"
                },
                "L  —  Playlist",
                "I  —  Media details",
                "Esc  —  Close panel or exit fullscreen",
                "Esc  —  Stop slideshow",
            ] {
                rows.push(note(label));
            }
            "KEYBOARD SHORTCUTS"
        }
    };
    let row_height = pixels(28);
    let header = pixels(if menu == Menu::SubtitleFont { 70 } else { 34 });
    let footer = pixels(match menu {
        Menu::Playlist | Menu::Slideshow => 42,
        Menu::Subtitles => 111,
        _ => 8,
    });
    let available = match menu {
        Menu::Context => v.h - 16 - header - footer,
        Menu::Slideshow => v.h - bottom_height(v) - slideshow_anchor(v).y - 36 - header - footer,
        _ => v.h - bottom_height(v) - 48 - header - pixels(6) - footer,
    };
    let max_rows = (available / row_height).clamp(1, 12) as usize;
    let max_scroll = rows.len().saturating_sub(max_rows);
    let scroll = v.scroll.min(max_scroll);
    let end = (scroll + max_rows).min(rows.len());
    let height = (end - scroll) as i32 * row_height + header + footer;
    let (x, y) = match menu {
        Menu::Open => (OPEN_LEFT, 48),
        Menu::Slideshow => {
            let anchor = slideshow_anchor(v);
            (
                (anchor.x + anchor.w - width).clamp(9, v.w - width - 9),
                (anchor.y + anchor.h + 5).min((v.h - bottom_height(v) - height - 5).max(48)),
            )
        }
        Menu::Context => {
            let (x, y) = v.context_anchor.unwrap_or(v.mouse);
            (
                x.clamp(8, (v.w - width - 8).max(8)),
                y.clamp(8, (v.h - height - 8).max(8)),
            )
        }
        _ => (
            v.w - width - pixels(10),
            (v.h - bottom_height(v) - height - pixels(6)).max(48),
        ),
    };
    f.rect(Rect::new(x, y, width, height), pixels(9), LINE, 0);
    f.rect(
        Rect::new(x + 1, y + 1, width - 2, height - 2),
        pixels(8),
        PANEL,
        0,
    );
    f.text(
        x + pixels(12),
        y + pixels(11),
        pixels(10),
        7,
        ACCENT,
        true,
        title,
    );
    f.hit(Rect::new(x, y, width, height), Action::Scroll(0));
    if max_scroll > 0 {
        f.icon_button(
            Rect::new(x + width - pixels(74), y + 2, pixels(23), pixels(23)),
            "ChevronUp",
            v,
            false,
            scroll > 0,
            Action::Scroll(-1),
        );
        f.icon_button(
            Rect::new(x + width - pixels(49), y + 2, pixels(23), pixels(23)),
            "ChevronDown",
            v,
            false,
            scroll < max_scroll,
            Action::Scroll(1),
        );
    }
    f.icon_button(
        Rect::new(x + width - pixels(25), y + 2, pixels(23), pixels(23)),
        "X",
        v,
        false,
        true,
        Action::Escape,
    );
    if menu == Menu::Playlist && max_scroll == 0 {
        f.text(
            x + width - pixels(38),
            y + pixels(11),
            pixels(10),
            9,
            MUTED,
            false,
            &format!("{} items", v.media.playlist.len()),
        );
    }
    if menu == Menu::SubtitleFont {
        let search = Rect::new(
            x + pixels(10),
            y + pixels(29),
            width - pixels(20),
            pixels(30),
        );
        f.rect(search, 5, LINE, 0);
        f.rect(
            Rect::new(search.x + 1, search.y + 1, search.w - 2, search.h - 2),
            4,
            "211A16",
            0,
        );
        let limit = ((search.w - pixels(16)) / pixels(7)).max(4) as usize;
        let text = if v.font_query.is_empty() {
            shorten("Type to search fonts…", limit)
        } else {
            let start = v
                .font_query
                .chars()
                .count()
                .saturating_sub(limit.saturating_sub(2));
            let query: String = v.font_query.chars().skip(start).collect();
            format!("{}{query}│", if start > 0 { "…" } else { "" })
        };
        f.text(
            search.x + pixels(8),
            search.y + search.h / 2,
            pixels(12),
            4,
            if v.font_query.is_empty() { MUTED } else { TEXT },
            false,
            &text,
        );
    }
    for (i, item) in rows.into_iter().skip(scroll).take(end - scroll).enumerate() {
        let row_y = y + header - pixels(4) + i as i32 * row_height;
        let r = Rect::new(x + pixels(5), row_y, width - pixels(10), pixels(26));
        if item.speed_stepper {
            speed_buttons(f, v, r);
            continue;
        }
        if item.active || (r.contains(v.mouse) && item.action.is_some()) {
            f.rect(
                r,
                pixels(5),
                if item.active { "39271D" } else { "31251D" },
                0,
            );
        }
        let reserved = pixels(if item.remove.is_some() {
            65
        } else if item.active {
            38
        } else {
            28
        });
        let limit = (((width - reserved) * 100 / PERCENT) / 7).max(4) as usize;
        f.text(
            x + pixels(13),
            row_y + pixels(5),
            pixels(12),
            7,
            if item.action.is_none() { MUTED } else { TEXT },
            menu == Menu::Playlist && item.active,
            &shorten(&item.label, limit),
        );
        if let Some(action) = item.action {
            f.hit(r, action);
        }
        if item.active {
            f.icon(
                x + width - pixels(if item.remove.is_some() { 39 } else { 18 }),
                row_y + pixels(13),
                pixels(14),
                "Check",
                ACCENT,
            );
        }
        if let Some(i) = item.remove {
            f.icon_button(
                Rect::new(x + width - pixels(30), row_y + 1, pixels(23), pixels(24)),
                "X",
                v,
                false,
                true,
                Action::Remove(i),
            );
        }
    }
    if menu == Menu::Subtitles {
        subtitle_options(f, v, x, y + height - footer, width);
    } else if menu == Menu::Slideshow {
        f.labeled_button(
            Rect::new(
                x + width - pixels(91),
                y + height - pixels(36),
                pixels(80),
                pixels(29),
            ),
            "Play",
            (Some("Play"), None),
            v,
            (!v.picking_music, !v.picking_music),
            Action::StartSlideshow,
        );
    } else if menu == Menu::Playlist {
        let wide = (width - pixels(22)) / 4;
        for (i, label, action, enabled) in [
            (0, "Add files", Action::AddFiles, true),
            (1, "Save", Action::Save, !v.media.playlist.is_empty()),
            (2, "Load", Action::Load, true),
            (3, "Clear", Action::Clear, !v.media.playlist.is_empty()),
        ] {
            f.button(
                Rect::new(
                    x + pixels(6) + i * (wide + pixels(3)),
                    y + height - pixels(35),
                    wide,
                    pixels(28),
                ),
                label,
                v,
                (false, enabled),
                action,
            );
        }
    }
}

fn subtitle_options(f: &mut Frame, v: View<'_>, x: i32, y: i32, width: i32) {
    let left = x + pixels(10);
    let group = pixels(98).min(width - pixels(20));
    let start = x + width - group - pixels(10);
    let label = if v.subtitle_direction.is_none() {
        "Direction · Auto"
    } else {
        "Direction"
    };
    if start - left >= pixels(66) {
        f.text(
            left,
            y + pixels(18),
            pixels(11),
            4,
            MUTED,
            false,
            &shorten(label, ((start - left) / pixels(6)) as usize),
        );
    }
    let top = y + pixels(3);
    let height = pixels(30);
    f.rect(Rect::new(start, top, group, height), 6, LINE, 0);
    f.rect(
        Rect::new(start + 1, top + 1, group - 2, height - 2),
        5,
        "211A16",
        0,
    );
    let width_ltr = (group - 2) / 2;
    for (offset, size, direction) in [
        (1, width_ltr, Direction::Ltr),
        (1 + width_ltr, group - 2 - width_ltr, Direction::Rtl),
    ] {
        let part = Rect::new(start + offset, top + 1, size, height - 2);
        let active = v.subtitle_direction == Some(direction);
        if active || part.contains(v.mouse) {
            f.rect(part, 4, if active { "3D2A1E" } else { "31251D" }, 0);
        }
        if offset > 1 {
            f.rect(Rect::new(part.x, top + 6, 1, height - 12), 0, LINE, 0);
        }
        f.text(
            part.x + size / 2,
            top + height / 2,
            pixels(12),
            5,
            if active { ACCENT } else { TEXT },
            active,
            direction.label(),
        );
        f.hit(part, Action::SubtitleDirection(direction));
    }
    for (offset, label, menu) in [
        (
            39,
            format!("Encoding: {}", v.subtitle_encoding.label),
            Menu::SubtitleEncoding,
        ),
        (
            73,
            format!("Font: {}", v.subtitle_font.unwrap_or("System default")),
            Menu::SubtitleFont,
        ),
    ] {
        let button = Rect::new(left, y + pixels(offset), width - pixels(20), pixels(29));
        f.labeled_button(
            button,
            &shorten(
                &label,
                ((button.w - pixels(34)) / pixels(7)).max(6) as usize,
            ),
            (None, Some("ChevronDown")),
            v,
            (false, true),
            Action::Menu(menu),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photos_have_a_segmented_zoom_control_and_a_slideshow_dropdown() {
        let media = Media {
            kind: Kind::Image,
            ..Default::default()
        };
        let mut v = video(&media);
        v.photo_position = Some((0, 3));
        v.live_available = true;
        for width in [1100, 320] {
            v.w = width;
            let frame = draw(v);
            let (w, h) = frame.resolution();
            let compact = w < COMPACT_PHOTO_WIDTH;
            let x = if compact { 9 } else { w - 237 };
            let y = h - if compact { 23 } else { 24 };
            for (offset, zoom) in [
                (19, PhotoZoom::Fit),
                (55, PhotoZoom::Fill),
                (96, PhotoZoom::Actual),
            ] {
                assert_eq!(
                    frame.at(frame.layout.physical_point((x + offset, y))),
                    Action::PhotoZoom(zoom)
                );
            }
            let anchor = slideshow_anchor(View { w, h, ..v });
            assert_eq!(
                frame.at(frame.layout.physical_point((anchor.x + 25, anchor.y + 12))),
                Action::Menu(Menu::Slideshow)
            );
        }
        v.w = 1100;
        v.menu = Some(Menu::Slideshow);
        let frame = draw(v);
        assert!(frame.hits.iter().any(|(_, a)| *a == Action::SlideshowMusic));
        assert!(frame.hits.iter().any(|(_, a)| *a == Action::StartSlideshow));
        v.picking_music = true;
        let frame = draw(v);
        assert!(!frame.hits.iter().any(|(_, a)| *a == Action::StartSlideshow));
    }

    #[test]
    fn slideshow_chrome_and_its_hit_targets_hide_together() {
        let media = Media {
            kind: Kind::Image,
            ..Default::default()
        };
        let mut v = video(&media);
        v.photo_position = Some((1, 3));
        v.slideshow_playing = true;
        v.live_available = true;
        v.controls_opacity = 0;
        v.controls_interactive = false;
        let hidden = draw(v);
        assert!(hidden.osd().is_empty());
        assert!(hidden.hits.is_empty());
        v.controls_opacity = 255;
        v.controls_interactive = true;
        let shown = draw(v);
        assert!(
            shown
                .hits
                .iter()
                .any(|(_, action)| *action == Action::StopSlideshow)
        );
    }

    fn video(media: &Media) -> View<'_> {
        View {
            w: 1100,
            h: 660,
            backing_scale: 1.0,
            mouse: (0, 0),
            media,
            photo_position: None,
            live_available: false,
            live_playing: false,
            prepare: None,
            timeline: None,
            caption: None,
            caption_style: CaptionStyle::default(),
            subtitle_direction: None,
            subtitle_encoding: ENCODINGS[0],
            subtitle_font: None,
            font_catalog: &Catalog::Unloaded,
            font_query: "",
            photo_zoom: PhotoZoom::Fit,
            slideshow_playing: false,
            slideshow_music: None,
            picking_music: false,
            stream: None,
            paused: false,
            muted: false,
            repeat_one: false,
            fullscreen: false,
            native_titlebar: false,
            resize_grips: false,
            controls_interactive: true,
            controls_opacity: 255,
            buffering: false,
            volume: 80.0,
            position: 10.0,
            duration: 60.0,
            buffered: 55.0,
            menu: None,
            context_anchor: None,
            scroll: 0,
            hdr: Hdr::Auto,
            devices: &[],
            decoder: DecodeChoice::Auto,
            render_mode: RenderMode::Auto,
            fast_render: false,
            speed: 1.0,
            seekable: true,
            quality: "max",
            toast: "",
        }
    }

    #[test]
    fn subtitle_controls_are_segmented_and_encoding_menu_selects_codepages() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        let mut v = video(&media);
        v.menu = Some(Menu::Subtitles);
        let frame = draw(v);
        let hit = |target| frame.hits.iter().find(|(_, a)| *a == target).unwrap().0;
        let ltr = hit(Action::SubtitleDirection(Direction::Ltr));
        let rtl = hit(Action::SubtitleDirection(Direction::Rtl));
        assert_eq!(ltr.x + ltr.w, rtl.x);
        assert_eq!(ltr.y, rtl.y);
        assert!(hit(Action::Menu(Menu::SubtitleEncoding)).y > rtl.y + rtl.h);
        assert!(
            hit(Action::Menu(Menu::SubtitleFont)).y > hit(Action::Menu(Menu::SubtitleEncoding)).y
        );
        v.menu = Some(Menu::SubtitleEncoding);
        let frame = draw(v);
        assert!(frame.hits.iter().any(
            |(_, action)| matches!(action, Action::SubtitleEncoding(e) if e.label.contains("1256"))
        ));
    }

    #[test]
    fn font_picker_keeps_original_indices_when_filtered() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        let catalog = Catalog::Ready(vec!["DejaVu Sans".into(), "Noto Sans Arabic".into()]);
        let mut v = video(&media);
        v.menu = Some(Menu::SubtitleFont);
        v.font_catalog = &catalog;
        let unfiltered = draw(v);
        assert!(
            unfiltered
                .hits
                .iter()
                .any(|(_, action)| *action == Action::SubtitleFont(None))
        );
        v.font_query = "ARABIC";
        let filtered = draw(v);
        assert!(
            filtered
                .hits
                .iter()
                .any(|(_, action)| *action == Action::SubtitleFont(Some(1)))
        );
        assert!(
            !filtered
                .hits
                .iter()
                .any(|(_, action)| *action == Action::SubtitleFont(Some(0)))
        );
    }

    #[test]
    fn directed_subtitle_inherits_the_selected_mpv_font() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        let mut v = video(&media);
        v.caption = Some("test caption");
        let frame = draw(v);
        let cue = frame
            .osd()
            .lines()
            .find(|line| line.ends_with("test caption"))
            .unwrap();
        assert!(cue.contains(r"\fs31.67\bord1.38"));
        assert!(!cue.contains(r"\fn"));
    }

    #[test]
    fn subtitles_rise_when_the_bar_is_visible_in_wide_and_compact_views() {
        for (width, height) in [(1100, 660), (480, 440)] {
            let (w, h) = Layout::new(width, height, 1.0).logical;
            let pos = subtitle_position((width, height), 1.0, true);
            assert!(i32::from(100 - pos) * h / 100 >= video_bar_height(w));
            assert_eq!(subtitle_position((width, height), 1.0, false), 100);
        }
    }

    #[test]
    fn prepare_panel_can_start_cancel_and_play_the_cached_copy() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        let mut v = video(&media);
        let mut p = crate::prepare::Progress::default();
        p.done = 5.0;
        p.total = 10.0;
        let status = Status::Running(Method::Gpu, p);
        v.menu = Some(Menu::Prepare);
        for (status, action) in [
            (None, Action::StartPreparation),
            (Some(&status), Action::CancelPreparation),
            (Some(&Status::Ready), Action::PlayPrepared),
        ] {
            v.prepare = Some(PrepareView {
                cached: false,
                legacy: false,
                playing: false,
                file_name: "sample-coffee.mp4",
                status,
            });
            let frame = draw(v);
            assert!(frame.hits.iter().any(|(_, a)| *a == action));
            if action == Action::CancelPreparation {
                assert!(frame.osd().contains("50%"));
                assert!(frame.osd().contains("ETA"));
            }
        }
    }

    #[test]
    fn scaled_volume_hitbox_follows_the_track() {
        let media = Media {
            kind: Kind::Video,
            ..Media::default()
        };
        let frame = draw(video(&media));
        assert_eq!(frame.resolution(), (1000, 600));
        let point = |x, y| frame.layout.physical_point((x, y));
        assert_eq!(frame.at(point(254, 574)), Action::SetVolume(0));
        assert_eq!(frame.at(point(283, 574)), Action::SetVolume(50));
        assert_eq!(frame.at(point(312, 574)), Action::SetVolume(100));
        assert_eq!(frame.at(point(230, 574)), Action::Mute);
        assert_eq!(frame.volume_at(point(345, 400), false), None);
        assert_eq!(frame.volume_at(point(345, 400), true), Some(100));
        assert_eq!(frame.volume_at(point(190, 400), true), Some(0));
    }

    #[test]
    fn repeat_one_is_left_of_volume_and_highlights_when_enabled() {
        for kind in [Kind::Video, Kind::Music] {
            let media = Media {
                kind,
                ..Media::default()
            };
            let mut view = video(&media);
            for width in [1100, 280] {
                view.w = width;
                let off = draw(view);
                let (repeat, _) = off
                    .hits
                    .iter()
                    .find(|(_, a)| *a == Action::RepeatOne)
                    .unwrap();
                let (volume, _) = off.hits.iter().find(|(_, a)| *a == Action::Mute).unwrap();
                assert!(repeat.x + repeat.w <= volume.x);
                assert_eq!(repeat.y, volume.y);

                view.repeat_one = true;
                assert_ne!(off.osd(), draw(view).osd());
                view.repeat_one = false;
            }
            view.live_playing = true;
            assert!(!draw(view).hits.iter().any(|(_, a)| *a == Action::RepeatOne));
        }
    }

    #[test]
    fn playback_menu_combines_speed_hdr_decoder_and_rendering() {
        let media = Media {
            kind: Kind::Video,
            ..Media::default()
        };
        let mut view = video(&media);
        let toolbar = draw(view);
        assert!(toolbar.osd().contains(icon_path("Settings", 19)));
        assert!(!toolbar.osd().lines().any(|line| line.ends_with("Playback")));
        view.menu = Some(Menu::Playback);
        let frame = draw(view);
        let (bounds, _) = frame
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Scroll(0))
            .unwrap();
        assert_eq!(bounds.w, pixels(PLAYBACK_WIDTH));
        for (label, menu) in [
            ("Video decoder", Menu::Decoder),
            ("Rendering quality", Menu::Rendering),
        ] {
            assert!(
                frame
                    .osd()
                    .lines()
                    .any(|line| line.contains(r"\fs13") && line.ends_with(label))
            );
            let (r, _) = frame
                .hits
                .iter()
                .find(|(_, a)| *a == Action::Menu(menu))
                .unwrap();
            let point = frame.layout.physical_point((r.x + 25, r.y + 10));
            assert_eq!(frame.at(point), Action::Menu(menu));

            view.menu = Some(menu);
            let submenu = draw(view);
            assert!(
                submenu
                    .osd()
                    .lines()
                    .any(|line| line.ends_with("Back to Playback"))
            );
            assert!(
                submenu
                    .hits
                    .iter()
                    .any(|(_, action)| *action == Action::Menu(Menu::Playback))
            );
        }
        for action in [
            Action::Speed(SpeedStep::Decrease),
            Action::Speed(SpeedStep::Increase),
            Action::Hdr(Hdr::Prefer),
        ] {
            assert!(frame.hits.iter().any(|(_, a)| *a == action));
        }
        view.menu = Some(Menu::Playback);
        view.speed = 0.25;
        view.seekable = false;
        assert!(
            !draw(view)
                .hits
                .iter()
                .any(|(_, a)| *a == Action::Speed(SpeedStep::Decrease))
        );
        assert!(frame.osd().lines().any(|line| line.ends_with("1.0×")));
        assert_eq!(speed_label(1.25), "1.25×");
        assert_eq!(speed_label(1.5), "1.5×");
        assert_eq!(speed_label(-64.0), "−64.0×");
    }

    #[test]
    fn timeline_preview_uses_the_same_track_as_seek_and_stays_inside_window() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        for width in [1100, 420] {
            let mut v = video(&media);
            v.w = width;
            let frame = draw(v);
            let (w, h) = frame.resolution();
            let bar = h - video_bar_height(w);
            for x in [14, w / 2, w - 14] {
                let point = frame.layout.physical_point((x, bar + 9));
                let hover = seek_hover((width, v.h), v.backing_scale, point).unwrap();
                assert_eq!(frame.at(point), Action::Seek(hover.fraction));
                let (x, y, image_w, image_h) = hover.image_bounds;
                assert!(x >= 0 && x + image_w <= width && y >= 0 && y + image_h < v.h);
                if width == 1100 {
                    assert_eq!(hover.box_rect.w, 400);
                    assert!(hover.image.w >= 2 * 192 && hover.image.h >= 2 * 108);
                } else {
                    assert!(hover.box_rect.w < 400);
                }
            }
            assert!(
                seek_hover(
                    (width, v.h),
                    v.backing_scale,
                    frame.layout.physical_point((w / 2, bar + 33))
                )
                .is_none()
            );
        }
    }

    #[test]
    fn retina_overlay_and_hit_targets_keep_their_size_in_points() {
        let media = Media {
            kind: Kind::Video,
            ..Default::default()
        };
        let mut v = video(&media);
        let standard = draw(v);
        v.w *= 2;
        v.h *= 2;
        v.backing_scale = 2.0;
        let retina = draw(v);
        assert_eq!(standard.resolution(), retina.resolution());
        let (w, h) = retina.resolution();
        let center = standard
            .layout
            .physical_point((w / 2, h - video_bar_height(w) + 9));
        let doubled = (center.0 * 2, center.1 * 2);
        assert_eq!(standard.at(center), retina.at(doubled));
        let hover = seek_hover((v.w, v.h), v.backing_scale, doubled).unwrap();
        let baseline = seek_hover((v.w / 2, v.h / 2), 1.0, center).unwrap();
        assert_eq!(retina.at(doubled), Action::Seek(hover.fraction));
        assert_eq!(
            (hover.image.w, hover.image.h),
            (baseline.image.w, baseline.image.h)
        );
        // Both edges are rounded separately to keep the bitmap aligned.
        assert!((hover.image_bounds.2 - 2 * baseline.image_bounds.2).abs() <= 2);
        assert!((hover.image_bounds.3 - 2 * baseline.image_bounds.3).abs() <= 2);
        assert_eq!(
            subtitle_position((v.w, v.h), 2.0, true),
            subtitle_position((v.w / 2, v.h / 2), 1.0, true)
        );
    }

    #[test]
    fn bare_bars_and_video_have_distinct_click_targets() {
        let media = Media {
            kind: Kind::Video,
            ..Media::default()
        };
        let frame = draw(video(&media));
        let point = |x, y| frame.layout.physical_point((x, y));
        assert_eq!(frame.at(point(500, 20)), Action::DragWindow);
        assert_eq!(frame.at(point(500, 250)), Action::ClickStage);
        assert_eq!(frame.at(point(500, 575)), Action::ClickBar);
        assert_eq!(frame.at(point(27, 575)), Action::Play);
    }

    #[test]
    fn native_titlebar_owns_window_buttons_and_media_info_has_a_play_frame() {
        let media = Media {
            kind: Kind::Video,
            ..Media::default()
        };
        let mut view = video(&media);
        view.native_titlebar = true;
        let frame = draw(view);
        assert!(frame.osd().contains(icon_path("SquarePlay", 19)));
        assert!(
            frame
                .hits
                .iter()
                .any(|(_, action)| *action == Action::Menu(Menu::Info))
        );
        assert!(!frame.hits.iter().any(|(_, action)| matches!(
            action,
            Action::Minimize | Action::Maximize | Action::Quit
        )));
    }

    #[test]
    fn icons_align_to_physical_pixels_without_stretching() {
        let side = 19;
        for (w, h, backing_scale) in [(1049, 631, 1.0), (2099, 1263, 2.0)] {
            let layout = Layout::new(w, h, backing_scale);
            let (origin, zoom) = layout.pixel_aligned_icon((300, 444), side);
            let check = |offset: f64, zoom: f64, physical: i32, logical: i32| {
                let scale = f64::from(physical) / f64::from(logical);
                let start = offset * scale;
                assert!((start - start.round()).abs() < 0.0001);
                let actual_size = f64::from(side) * zoom / 100.0 * scale;
                assert!((actual_size - f64::from(pixels(side)) * backing_scale).abs() < 0.0001);
            };
            check(origin.0, zoom.0, layout.physical.0, layout.logical.0);
            check(origin.1, zoom.1, layout.physical.1, layout.logical.1);
        }
    }

    #[test]
    fn context_menu_stays_in_window_and_current_playlist_entry_is_bold() {
        let media = Media {
            kind: Kind::Video,
            path: "/media/current.mkv".into(),
            playlist: vec!["/media/first.mkv".into(), "/media/current.mkv".into()],
            index: Some(1),
            ..Media::default()
        };
        let mut view = video(&media);
        view.menu = Some(Menu::Context);
        view.context_anchor = Some((1099, 659));
        let frame = draw(view);
        let (bounds, _) = frame
            .hits
            .iter()
            .find(|(_, action)| *action == Action::Scroll(0))
            .unwrap();
        let point = frame
            .layout
            .physical_point((bounds.x + 15, bounds.y + pixels(43 + 56)));
        assert!(bounds.x + bounds.w < 1000 && bounds.y + bounds.h < 600);
        assert_eq!(frame.at(point), Action::ShowInPlaylist);
        view.menu = Some(Menu::Playlist);
        let frame = draw(view);
        assert!(
            frame
                .osd()
                .lines()
                .any(|line| line.contains(r"\b1") && line.ends_with("02  current.mkv"))
        );
    }

    #[test]
    fn fading_controls_share_artwork_opacity_and_stop_accepting_hits() {
        let media = Media {
            kind: Kind::Video,
            ..Media::default()
        };
        let mut view = video(&media);
        view.controls_interactive = false;
        view.controls_opacity = 128;
        let frame = draw(view);
        assert!(frame.osd().contains(r"\1a&H7F&"));
        assert!(
            frame.osd().lines().any(
                |line| line.contains(icon_path("FolderOpen", 14)) && line.contains(r"\1a&H7F&")
            )
        );
        assert!(
            frame
                .osd()
                .lines()
                .any(|line| line.contains(icon_path("Pause", 20)) && line.contains(r"\1a&H7F&"))
        );
        assert_eq!(
            frame.at(frame.layout.physical_point((248, 574))),
            Action::ClickStage
        );
        view.controls_opacity = 0;
        let frame = draw(view);
        assert!(frame.osd().is_empty());
    }

    #[test]
    fn open_button_and_menu_align_at_the_left_edge() {
        let media = Media::default();
        let mut view = video(&media);
        let frame = draw(view);
        assert_eq!(
            frame.at(frame.layout.physical_point((OPEN_LEFT + 10, 20))),
            Action::Menu(Menu::Open),
        );
        view.menu = Some(Menu::Open);
        let frame = draw(view);
        let (panel, _) = frame
            .hits
            .iter()
            .find(|(_, a)| *a == Action::Scroll(0))
            .unwrap();
        assert_eq!(panel.x, OPEN_LEFT);
    }
}
