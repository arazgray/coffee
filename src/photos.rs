use crate::{library, state::image_path};
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

pub struct PhotoFolder {
    files: Vec<PathBuf>,
    pub index: usize,
    pub live_clip: Option<PathBuf>,
}

impl PhotoFolder {
    pub fn scan(file: &str) -> Option<Self> {
        let file = PathBuf::from(library::normalize(file)?);
        if !file.is_absolute() {
            return None;
        }
        let stem = file.file_stem()?.to_string_lossy();
        let mut files = Vec::new();
        let mut live_clip = None;
        for entry in fs::read_dir(file.parent()?).ok()?.flatten() {
            let path = entry.path();
            if image_path(&path.to_string_lossy()) && path.is_file() {
                files.push(path);
            } else if path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("mov"))
                && path
                    .file_stem()
                    .is_some_and(|s| s.eq_ignore_ascii_case(stem.as_ref()))
                && path.is_file()
            {
                // Prefer the same spelling when case-sensitive folders
                // contain more than one possible companion.
                if live_clip.is_none() || path.file_stem() == file.file_stem() {
                    live_clip = Some(path);
                }
            }
        }
        files.sort_unstable_by(|a, b| natural_order(&name(a), &name(b)));
        let index = files.iter().position(|path| path == &file).or_else(|| {
            files
                .iter()
                .position(|path| path.file_name() == file.file_name())
        })?;
        Some(Self {
            files,
            index,
            live_clip,
        })
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn adjacent(&self, direction: i32) -> Option<&Path> {
        let index = self.index.checked_add_signed(direction as isize)?;
        self.files.get(index).map(PathBuf::as_path)
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
