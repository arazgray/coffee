use crate::engine::Engine;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hdr {
    Off,
    #[default]
    Auto,
    Prefer,
}

impl Hdr {
    pub fn apply(self, mpv: &Engine) -> Result<(), String> {
        let (hint, mode, transfer, primaries) = match self {
            Self::Off => ("no", "target", "srgb", "bt.709"),
            Self::Auto => ("auto", "target", "auto", "auto"),
            Self::Prefer => ("yes", "source", "auto", "auto"),
        };
        mpv.set("target-trc", transfer)?;
        mpv.set("target-prim", primaries)?;
        mpv.set("target-colorspace-hint-mode", mode)?;
        mpv.set("target-colorspace-hint", hint)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Empty,
    Video,
    Music,
    Image,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PhotoZoom {
    #[default]
    Fit,
    Fill,
    Actual,
}

impl PhotoZoom {
    pub fn options(self) -> [(&'static str, &'static str); 2] {
        [
            ("panscan", if self == Self::Fill { "1" } else { "0" }),
            (
                "video-unscaled",
                if self == Self::Actual { "yes" } else { "no" },
            ),
        ]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderMode {
    #[default]
    Auto,
    Quality,
    Fast,
}

fn high_rate_4k(width: Option<i64>, height: Option<i64>, fps: Option<f64>) -> bool {
    (width.is_some_and(|w| w >= 3840) || height.is_some_and(|h| h >= 2160))
        && fps.is_some_and(|fps| fps >= 49.0)
}

impl RenderMode {
    pub fn fast(self, high_rate_4k: bool) -> bool {
        match self {
            Self::Auto => high_rate_4k,
            Self::Quality => false,
            Self::Fast => true,
        }
    }

    pub fn apply(fast: bool, mpv: &Engine) -> Result<(), String> {
        // Leave the output color space, HDR hint, and dithering alone.
        for (key, lighter, quality) in [
            ("scale", "bilinear", "lanczos"),
            ("dscale", "bilinear", "hermite"),
            ("correct-downscaling", "no", "yes"),
            ("linear-downscaling", "no", "yes"),
            ("sigmoid-upscaling", "no", "yes"),
            ("hdr-compute-peak", "no", "auto"),
            ("allow-delayed-peak-detect", "yes", "no"),
        ] {
            mpv.set(key, if fast { lighter } else { quality })?;
        }
        Ok(())
    }
}

pub const PHOTO_EXTENSIONS: &str = "jpg jpeg jfif jpe png apng webp gif avif bmp tif tiff jxl heic heif heics heifs qoi ico jp2 j2k j2c tga dds hdr exr pbm pgm ppm pam";

pub fn image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| {
            PHOTO_EXTENSIONS
                .split_whitespace()
                .any(|ext| ext.eq_ignore_ascii_case(s))
        })
}

pub fn is_stream(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

pub fn display_name(path: &str) -> String {
    if is_stream(path) {
        if let Ok(url) = url::Url::parse(path) {
            return url
                .path_segments()
                .and_then(|mut items| items.next_back())
                .filter(|s| !s.is_empty())
                .or_else(|| url.host_str())
                .unwrap_or(path)
                .to_owned();
        }
    }
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_owned()
}

#[derive(Debug)]
pub struct Track {
    pub kind: String,
    pub id: i64,
    pub title: String,
    pub selected: bool,
    pub cover: bool,
}

#[derive(Default)]
pub struct Media {
    pub path: String,
    pub title: String,
    pub kind: Kind,
    pub cover: bool,
    pub track: String,
    pub artist: String,
    pub album: String,
    pub infos: Vec<(String, String)>,
    pub tracks: Vec<Track>,
    pub playlist: Vec<String>,
    pub index: Option<usize>,
    pub source_range: String,
    pub output_range: String,
    pub decoder: String,
    pub software_av1: bool,
    pub high_rate_4k: bool,
}

fn string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn label(value: String) -> String {
    if value.is_empty() {
        "Not reported".into()
    } else {
        value
    }
}

fn range(gamma: &Value) -> String {
    match gamma.as_str().unwrap_or_default() {
        "pq" | "st2084" | "smpte2084" => "HDR (PQ)".into(),
        "hlg" | "arib-std-b67" => "HDR (HLG)".into(),
        "" | "auto" | "unknown" => "Not reported".into(),
        _ => "SDR".into(),
    }
}

impl Media {
    pub fn containing_folder(&self) -> Option<PathBuf> {
        let path = PathBuf::from(crate::library::normalize(&self.path)?);
        path.is_absolute()
            .then(|| path.parent().map(Path::to_owned))
            .flatten()
    }

    pub fn read(mpv: &Engine) -> Self {
        let path = string(&mpv.get("path"));
        let source = mpv.get("video-params");
        let target = mpv.get("video-target-params");
        let raw_tracks = mpv.get("track-list");
        let tracks: Vec<_> = raw_tracks
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let kind = item["type"].as_str()?;
                let id = item["id"].as_i64()?;
                let mut text = [
                    item["title"].as_str(),
                    item["lang"].as_str(),
                    item["codec"].as_str(),
                ]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
                if text.is_empty() {
                    text = format!("Track {id}");
                }
                Some(Track {
                    kind: kind.to_owned(),
                    id,
                    title: text,
                    selected: item["selected"].as_bool().unwrap_or(false),
                    cover: item["albumart"].as_bool().unwrap_or(false),
                })
            })
            .collect();
        let cover = tracks.iter().any(|item| item.cover);
        let title = display_name(&path);
        let fallback_track = Path::new(&title)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&title);
        let kind = if path.is_empty() {
            Kind::Empty
        } else if image_path(&path) {
            Kind::Image
        } else if tracks.iter().any(|item| item.kind == "audio")
            && !tracks
                .iter()
                .any(|item| item.kind == "video" && !item.cover)
        {
            Kind::Music
        } else {
            Kind::Video
        };
        let meta = mpv.get("metadata");
        let tag = |key: &str| {
            meta.as_object().and_then(|m| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .and_then(|(_, v)| v.as_str().filter(|s| !s.is_empty()))
            })
        };
        let track = tag("title").unwrap_or(fallback_track).to_owned();
        let artist = tag("artist")
            .or_else(|| tag("album_artist"))
            .unwrap_or("Unknown artist")
            .to_owned();
        let album = tag("album").unwrap_or("Unknown album").to_owned();
        let video = raw_tracks
            .as_array()
            .and_then(|tracks| {
                tracks.iter().find(|track| {
                    track["type"].as_str() == Some("video")
                        && track["selected"].as_bool() == Some(true)
                })
            })
            .unwrap_or(&Value::Null);
        let width = source["w"].as_i64().or_else(|| video["demux-w"].as_i64());
        let height = source["h"].as_i64().or_else(|| video["demux-h"].as_i64());
        let resolution = match (width, height) {
            (Some(w), Some(h)) => format!("{w} × {h}"),
            _ => "Not reported".into(),
        };
        let fps = mpv
            .get("container-fps")
            .as_f64()
            .filter(|n| *n > 0.0)
            .or_else(|| video["demux-fps"].as_f64().filter(|n| *n > 0.0))
            .or_else(|| mpv.get("estimated-vf-fps").as_f64().filter(|n| *n > 0.0));
        let high_rate_4k = kind == Kind::Video && high_rate_4k(width, height, fps);
        let fps = fps.map_or_else(|| "Not reported".into(), |n| format!("{n:.2} fps"));
        let source_range = range(&source["gamma"]);
        let output_range = range(&target["gamma"]);
        let hwdec = string(&mpv.get("hwdec-current"));
        let software_av1 = hwdec == "no" && video["codec"].as_str() == Some("av1");
        let decoder = match hwdec.as_str() {
            "" => "Not reported".into(),
            "no" if video["decoder"]
                .as_str()
                .is_some_and(|s| s.contains("libdav1d")) =>
            {
                "Software (dav1d)".into()
            }
            "no" => "Software".into(),
            "vaapi" => "VA-API".into(),
            "vaapi-copy" => "VA-API (copy)".into(),
            "nvdec" => "NVDEC".into(),
            "nvdec-copy" => "NVDEC (copy)".into(),
            _ => hwdec,
        };
        let infos = match kind {
            Kind::Image => vec![
                ("Format".into(), label(string(&mpv.get("file-format")))),
                ("Resolution".into(), resolution),
            ],
            Kind::Music => vec![
                ("Format".into(), label(string(&mpv.get("file-format")))),
                (
                    "Audio codec".into(),
                    label(string(&mpv.get("audio-codec-name"))),
                ),
                (
                    "Duration".into(),
                    time(mpv.get("duration").as_f64().unwrap_or(0.0)),
                ),
                ("Artist".into(), artist.clone()),
                ("Album".into(), album.clone()),
            ],
            _ => vec![
                ("Format".into(), label(string(&mpv.get("file-format")))),
                ("Video codec".into(), label(string(&video["codec-desc"]))),
                (
                    "Audio codec".into(),
                    label(string(&mpv.get("audio-codec-name"))),
                ),
                ("Frame rate".into(), fps),
                ("Resolution".into(), resolution),
                ("Source range".into(), source_range.clone()),
                ("Display output".into(), output_range.clone()),
                ("Decoder".into(), decoder.clone()),
                (
                    "Dropped frames".into(),
                    mpv.get("frame-drop-count")
                        .as_i64()
                        .map_or_else(|| "Not reported".into(), |n| n.to_string()),
                ),
                (
                    "Duration".into(),
                    time(mpv.get("duration").as_f64().unwrap_or(0.0)),
                ),
            ],
        };
        let playlist = mpv
            .get("playlist")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["filename"].as_str().map(str::to_owned))
            .collect();
        let index = mpv
            .get("playlist-pos")
            .as_i64()
            .and_then(|i| usize::try_from(i).ok());
        Self {
            path,
            title,
            kind,
            cover,
            track,
            artist,
            album,
            infos,
            tracks,
            playlist,
            index,
            source_range,
            output_range,
            decoder,
            software_av1,
            high_rate_4k,
        }
    }
}

pub fn time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".into();
    }
    let seconds = seconds as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_profile_is_fast_by_default_only_for_high_rate_4k() {
        let rate = |w, h, f| high_rate_4k(Some(w), Some(h), Some(f));
        assert!(RenderMode::Auto.fast(rate(3840, 2160, 59.94)));
        assert!(RenderMode::Auto.fast(rate(4096, 1716, 50.)));
        assert!(!RenderMode::Auto.fast(rate(3840, 2160, 24.)));
        assert!(!RenderMode::Auto.fast(rate(1920, 1080, 60.)));
        assert!(!RenderMode::Auto.fast(high_rate_4k(Some(3840), Some(2160), None)));
        assert!(!RenderMode::Quality.fast(true));
        assert!(RenderMode::Fast.fast(false));
    }
}
