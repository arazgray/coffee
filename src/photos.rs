use crate::{library, state::image_path};
use std::{
    cmp::Ordering,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

pub struct PhotoFolder {
    files: Vec<PathBuf>,
    pub index: usize,
    clips: HashMap<String, Vec<PathBuf>>,
}

impl PhotoFolder {
    pub fn scan(file: &str) -> Option<Self> {
        let file = PathBuf::from(library::normalize(file)?);
        if !file.is_absolute() {
            return None;
        }
        let mut files = Vec::new();
        let mut clips = HashMap::<_, Vec<_>>::new();
        for entry in fs::read_dir(file.parent()?).ok()?.flatten() {
            let path = entry.path();
            if image_path(&path.to_string_lossy()) && path.is_file() {
                files.push(path);
            } else if path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("mov"))
                && path.is_file()
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                clips
                    .entry(stem.to_ascii_lowercase())
                    .or_default()
                    .push(path);
            }
        }
        files.sort_unstable_by(|a, b| natural_order(&name(a), &name(b)));
        let mut folder = Self {
            files,
            index: 0,
            clips,
        };
        folder.select(&file).then_some(folder)
    }

    pub fn select(&mut self, file: &Path) -> bool {
        let Some(index) = self.files.iter().position(|path| path == file).or_else(|| {
            // File names are case-insensitive on Windows and on most Macs.
            #[cfg(any(windows, target_os = "macos"))]
            if self
                .files
                .first()
                .is_some_and(|first| first.parent() == file.parent())
            {
                return self
                    .files
                    .iter()
                    .position(|path| name(path).eq_ignore_ascii_case(&name(file)));
            }
            None
        }) else {
            return false;
        };
        self.index = index;
        true
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn live_clip(&self) -> Option<&Path> {
        let stem = self.files.get(self.index)?.file_stem()?.to_str()?;
        self.clips
            .get(&stem.to_ascii_lowercase())?
            .iter()
            // Prefer exact stem spelling, with a deterministic tie-breaker.
            .min_by_key(|path| {
                (
                    path.file_stem().and_then(|s| s.to_str()) != Some(stem),
                    *path,
                )
            })
            .map(PathBuf::as_path)
    }

    pub fn adjacent(&self, direction: i32) -> Option<&Path> {
        let index = self.index.checked_add_signed(direction as isize)?;
        self.files.get(index).map(PathBuf::as_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_image_siblings_and_matches_only_same_stem_movs() {
        let dir = std::env::temp_dir().join(format!("coffee-photos-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        for name in [
            "IMG_2.jpg",
            "IMG_3.HEIC",
            "IMG_10.qoi",
            "img_3.MoV",
            "IMG_2.mp4",
            "other.mov",
        ] {
            fs::write(dir.join(name), []).unwrap();
        }
        fs::create_dir(dir.join("not-a-file.png")).unwrap();
        let still = dir.join("IMG_3.HEIC");
        let mut folder = PhotoFolder::scan(still.to_str().unwrap()).unwrap();
        assert_eq!(folder.files().len(), 3);
        assert_eq!(folder.index, 1);
        assert_eq!(folder.adjacent(-1), Some(dir.join("IMG_2.jpg").as_path()));
        assert_eq!(folder.adjacent(1), Some(dir.join("IMG_10.qoi").as_path()));
        assert_eq!(folder.live_clip(), Some(dir.join("img_3.MoV").as_path()));
        assert!(folder.select(&dir.join("IMG_2.jpg")));
        assert!(folder.live_clip().is_none());
        assert!(folder.adjacent(-1).is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}

fn name(path: &Path) -> std::borrow::Cow<'_, str> {
    path.file_name().unwrap_or_default().to_string_lossy()
}

pub fn natural_order(a: &str, b: &str) -> Ordering {
    fn parts(s: &str) -> Vec<(&str, bool)> {
        let mut result = Vec::new();
        let mut start = 0;
        let mut numeric = None;
        for (i, c) in s.char_indices() {
            let current = c.is_ascii_digit();
            if let Some(was_numeric) = numeric
                && was_numeric != current
            {
                result.push((&s[start..i], was_numeric));
                start = i;
            }
            numeric = Some(current);
        }
        if let Some(is_numeric) = numeric {
            result.push((&s[start..], is_numeric));
        }
        result
    }
    let (left, right) = (parts(a), parts(b));
    for ((one, num_a), (two, num_b)) in left.iter().zip(&right) {
        let order = if *num_a && *num_b {
            let (one, two) = (one.trim_start_matches('0'), two.trim_start_matches('0'));
            one.len().cmp(&two.len()).then_with(|| one.cmp(two))
        } else {
            one.to_lowercase().cmp(&two.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len()).then_with(|| a.cmp(b))
}
