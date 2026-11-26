use crate::{library, state::image_path};
use std::{
    cmp::Ordering,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Default)]
pub struct PanBounds {
    image: [f64; 2],
    limit: [f64; 2],
}

impl PanBounds {
    pub fn from_margins(size: (i32, i32), margins: [i64; 4]) -> Self {
        let view = [f64::from(size.0.max(0)), f64::from(size.1.max(0))];
        // mpv reports negative margins when the full rendered frame extends
        // beyond the viewport. This includes rotation and the selected zoom.
        let image = [
            (view[0] - margins[0] as f64 - margins[1] as f64).max(1.0),
            (view[1] - margins[2] as f64 - margins[3] as f64).max(1.0),
        ];
        let limit = std::array::from_fn(|i| (image[i] - view[i]).max(0.0) / (2.0 * image[i]));
        Self { image, limit }
    }

    pub fn clamp(self, position: [f64; 2]) -> [f64; 2] {
        std::array::from_fn(|i| position[i].clamp(-self.limit[i], self.limit[i]))
    }

    pub fn drag(self, position: [f64; 2], delta: [f64; 2]) -> [f64; 2] {
        self.clamp(std::array::from_fn(|i| {
            position[i] + delta[i] / self.image[i].max(1.0)
        }))
    }
}

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
        self.live_clip_for(self.index)
    }

    fn live_clip_for(&self, index: usize) -> Option<&Path> {
        let stem = self.files.get(index)?.file_stem()?.to_str()?;
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

    pub fn preload_paths(&self) -> Vec<PathBuf> {
        let neighborhood = std::iter::once(self.index)
            .chain((1..=5).flat_map(|distance| {
                [
                    self.index.checked_sub(distance),
                    self.index.checked_add(distance),
                ]
                .into_iter()
                .flatten()
                .filter(|&i| i < self.files.len())
            }))
            .collect::<Vec<_>>();
        // Warm the current Live clip first, then the stills, then the clips
        // belonging to the neighboring shots.
        self.live_clip()
            .map(Path::to_path_buf)
            .into_iter()
            .chain(neighborhood.iter().map(|&i| self.files[i].clone()))
            .chain(
                neighborhood
                    .iter()
                    .skip(1)
                    .filter_map(|&i| self.live_clip_for(i).map(Path::to_path_buf)),
            )
            .collect()
    }

    pub fn adjacent(&self, direction: i32) -> Option<&Path> {
        let index = self.index.checked_add_signed(direction as isize)?;
        self.files.get(index).map(PathBuf::as_path)
    }

    pub fn next_slide(&self) -> Option<&Path> {
        (self.files.len() > 1).then(|| self.files[(self.index + 1) % self.files.len()].as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panning_tracks_the_drag_and_stops_at_each_crop_edge() {
        let fill = PanBounds::from_margins((800, 600), [-200, -200, 0, 0]);
        assert_eq!(fill.drag([0.0; 2], [120.0, 500.0]), [0.1, 0.0]);
        assert_eq!(fill.drag([0.0; 2], [-900.0, 500.0]), [-1.0 / 6.0, 0.0]);
        let actual = PanBounds::from_margins((800, 600), [-200, -200, -100, -100]);
        assert_eq!(actual.drag([0.0; 2], [400.0, 400.0]), [1.0 / 6.0, 0.125]);
        let fit = PanBounds::from_margins((800, 600), [0, 0, 100, 100]);
        assert_eq!(fit.drag([0.0; 2], [400.0, 400.0]), [0.0; 2]);
    }

    #[test]
    fn scans_image_siblings_and_matches_only_same_stem_movs() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
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
    }

    #[test]
    fn preloads_five_on_each_side_and_their_live_clips() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        for i in 0..15 {
            fs::write(dir.join(format!("Shot {i}.jpg")), []).unwrap();
            fs::write(dir.join(format!("Shot {i}.MOV")), []).unwrap();
        }
        let mut folder = PhotoFolder::scan(dir.join("Shot 7.jpg").to_str().unwrap()).unwrap();
        let paths = folder.preload_paths();
        assert_eq!(paths.len(), 22);
        assert_eq!(paths[0], dir.join("Shot 7.MOV"));
        assert_eq!(paths[1], dir.join("Shot 7.jpg"));
        for i in 0..15 {
            let expected = (2..=12).contains(&i);
            assert_eq!(paths.contains(&dir.join(format!("Shot {i}.jpg"))), expected);
            assert_eq!(paths.contains(&dir.join(format!("Shot {i}.MOV"))), expected);
        }
        assert!(folder.select(&dir.join("Shot 0.jpg")));
        assert_eq!(folder.preload_paths().len(), 12);
        assert_eq!(folder.next_slide(), Some(dir.join("Shot 1.jpg").as_path()));
        assert!(folder.select(&dir.join("Shot 14.jpg")));
        assert_eq!(folder.next_slide(), Some(dir.join("Shot 0.jpg").as_path()));
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
