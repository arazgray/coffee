use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub files: Vec<String>,
    pub current: usize,
}

pub fn normalize(input: &str) -> Option<String> {
    if input.is_empty() || input.contains('\0') {
        return None;
    }
    let path = Path::new(input);
    if path.is_absolute() {
        return Some(path.to_string_lossy().into_owned());
    }
    if let Ok(url) = url::Url::parse(input) {
        match url.scheme() {
            "file" => {
                return url
                    .to_file_path()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned());
            }
            "http" | "https" if url.host_str().is_some() => return Some(url.to_string()),
            _ => return None,
        }
    }
    let absolute = std::env::current_dir().ok()?.join(path);
    Some(absolute.to_string_lossy().into_owned())
}

pub fn load(path: &Path) -> Result<Playlist, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > 2 * 1024 * 1024 {
        return Err("That playlist is larger than 2 MiB".into());
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut result: Playlist = if text.trim().starts_with('{') {
        serde_json::from_str(&text).map_err(|e| format!("Invalid playlist: {e}"))?
    } else {
        Playlist {
            files: text
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .map(str::to_owned)
                .collect(),
            current: 0,
        }
    };
    let directory = path.parent().unwrap_or(Path::new("."));
    result.files = result
        .files
        .iter()
        .filter_map(|entry| {
            if url::Url::parse(entry).is_ok() || Path::new(entry).is_absolute() {
                normalize(entry)
            } else {
                directory.join(entry).to_str().and_then(normalize)
            }
        })
        .collect();
    if result.files.is_empty() {
        return Err("This playlist contains no playable paths or URLs".into());
    }
    result.current = result.current.min(result.files.len() - 1);
    Ok(result)
}

pub fn save(path: &Path, playlist: &Playlist) -> Result<PathBuf, String> {
    let actual = if path.extension().is_none() {
        path.with_extension("coffee.json")
    } else {
        path.to_owned()
    };
    let data = serde_json::to_vec_pretty(playlist).map_err(|e| e.to_string())?;
    fs::write(&actual, data).map_err(|e| e.to_string())?;
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_file_uris_and_rejects_non_media_protocols() {
        #[cfg(unix)]
        assert_eq!(
            normalize("file:///run/media/WD/one%20two.mp4").unwrap(),
            "/run/media/WD/one two.mp4"
        );
        #[cfg(windows)]
        assert_eq!(
            normalize("file:///C:/Media/one%20two.mp4").unwrap(),
            r"C:\Media\one two.mp4"
        );
        assert_eq!(
            normalize("https://example.org/media.m3u8").unwrap(),
            "https://example.org/media.m3u8"
        );
        assert!(normalize("javascript:alert(1)").is_none());
    }
}
