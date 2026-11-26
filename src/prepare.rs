//! Cancellable FFmpeg jobs. Sibling MP4s contain only the prepared video
//! track so mpv can keep the source audio, subtitles, chapters and queue.
use std::{
    env,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

struct Sample {
    at: Instant,
    position: f64,
    drops: u64,
}

#[derive(Default)]
pub struct PlaybackHealth {
    last: Option<Sample>,
    bad: u8,
    detected: bool,
}

impl PlaybackHealth {
    pub fn detected(&self) -> bool {
        self.detected
    }

    pub fn interrupt(&mut self) {
        self.last = None;
        self.bad = 0;
    }

    pub fn due(&self, now: Instant) -> bool {
        !self.detected
            && self
                .last
                .as_ref()
                .is_none_or(|last| now - last.at >= HEALTH_INTERVAL)
    }

    pub fn observe(&mut self, now: Instant, position: f64, speed: f64, fps: f64, drops: u64) {
        let Some(last) = self.last.replace(Sample {
            at: now,
            position,
            drops,
        }) else {
            return;
        };
        let elapsed = (now - last.at).as_secs_f64();
        let expected = elapsed * speed;
        let advanced = position - last.position;
        if !(0.8..=3.0).contains(&elapsed)
            || expected <= 0.0
            || advanced < -0.15
            || advanced > expected * 1.65 + 0.2
            || drops < last.drops
        {
            // Counter resets and seek/discontinuity jumps are not lag.
            self.bad = 0;
            return;
        }
        let min_drops = (fps * expected * 0.08).ceil().max(3.0) as u64;
        if drops - last.drops >= min_drops || advanced < expected * 0.55 {
            self.bad += 1;
            self.detected |= self.bad >= 2;
        } else {
            self.bad = 0;
        }
    }
}

#[derive(Clone)]
pub struct Target {
    source: PathBuf,
    pub output: PathBuf,
    stamp: u64,
    hdr: bool,
    av1: bool,
    pub cached: bool,
    pub legacy: Option<PathBuf>,
}

fn legacy_cache_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = PathBuf::from(env::var_os("LOCALAPPDATA")?);
    #[cfg(target_os = "macos")]
    let root = PathBuf::from(env::var_os("HOME")?).join("Library/Caches");
    #[cfg(target_os = "linux")]
    let root = env::var_os("XDG_CACHE_HOME")
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(root.join("coffee").join("prepared"))
}

fn fingerprint(source: &Path) -> Option<u64> {
    let meta = source.metadata().ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut hash = DefaultHasher::new();
    (source, meta.len(), meta.modified().ok()?).hash(&mut hash);
    Some(hash.finish())
}

fn source_stamp(source: &Path) -> Option<u64> {
    let meta = source.metadata().ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut hash = DefaultHasher::new();
    // Unlike the old cache key, this stays valid when a folder and its
    // side-by-side videos are moved together without changing the files.
    (source.file_name()?, meta.len(), meta.modified().ok()?).hash(&mut hash);
    Some(hash.finish())
}

// A conforming, ignorable MP4 uuid atom at EOF identifies our own copies
// without running ffprobe or creating a sidecar. The final 8 bytes bind
// the copy to a specific revision of its source file.
const BOX_HEADER: [u8; 24] = *b"\x00\x00\x00\x20uuidCoffeeSmooth1.2!";
const BOX_LEN: usize = BOX_HEADER.len() + 8;

fn prepared_marker(path: &Path) -> Option<u64> {
    let meta = path.symlink_metadata().ok()?;
    if !meta.is_file() || meta.len() < (BOX_LEN + 8) as u64 {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut header = [0; 8];
    file.read_exact(&mut header).ok()?;
    if &header[4..] != b"ftyp" {
        return None;
    }
    let mut marker = [0; BOX_LEN];
    file.seek(SeekFrom::End(-(BOX_LEN as i64))).ok()?;
    file.read_exact(&mut marker).ok()?;
    (marker[..BOX_HEADER.len()] == BOX_HEADER)
        .then(|| u64::from_be_bytes(marker[BOX_HEADER.len()..].try_into().unwrap()))
}

fn stamp_output(path: &Path, stamp: u64) -> io::Result<()> {
    let mut file = fs::OpenOptions::new().read(true).append(true).open(path)?;
    let mut header = [0; 8];
    file.rewind()?;
    file.read_exact(&mut header)?;
    if &header[4..] != b"ftyp" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FFmpeg did not produce an MP4",
        ));
    }
    file.write_all(&BOX_HEADER)?;
    file.write_all(&stamp.to_be_bytes())
}

fn sibling_output(path: &Path, stamp: u64) -> Option<(PathBuf, bool)> {
    let stem = path.file_stem()?;
    let directory = path.parent()?;
    // Inspect existing numbered names even if an earlier copy was removed.
    // Typically this needs only one directory scan plus a single file probe.
    let base = stem.to_str()?;
    let mut occupied = fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let tail = name.to_str()?.strip_prefix(base)?.strip_prefix("-coffee")?;
            let index = if tail == ".mp4" {
                1
            } else {
                let n: u32 = tail.strip_prefix('-')?.strip_suffix(".mp4")?.parse().ok()?;
                if n < 2 {
                    return None;
                }
                n
            };
            Some((index, entry.path()))
        })
        .collect::<Vec<_>>();
    occupied.sort_unstable_by_key(|(n, _)| *n);
    if let Some((_, existing)) = occupied
        .iter()
        .find(|(_, other)| prepared_marker(other) == Some(stamp))
    {
        return Some((existing.clone(), true));
    }
    // Probe the actual candidate names as well: on a case-insensitive
    // filesystem the collision might use different capitalization.
    for index in 1..=10000 {
        let suffix = if index == 1 {
            "-coffee.mp4".into()
        } else {
            format!("-coffee-{index}.mp4")
        };
        let mut name = stem.to_owned();
        name.push(suffix);
        let candidate = directory.join(name);
        match candidate.symlink_metadata() {
            Ok(_) if prepared_marker(&candidate) == Some(stamp) => return Some((candidate, true)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Some((candidate, false));
            }
            Err(_) => return None,
            Ok(_) => {}
        }
    }
    None
}

impl Target {
    pub fn local(path: &str, hdr: bool, av1: bool) -> Option<Self> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return None;
        }
        let source = path.canonicalize().ok()?;
        if prepared_marker(&source).is_some() {
            return None; // A prepared MP4 is already the light video track.
        }
        let stamp = source_stamp(&source)?;
        let (output, cached) = sibling_output(path, stamp)?;
        let legacy = (!cached)
            .then(|| {
                let key = fingerprint(&source)?;
                let file = legacy_cache_directory()?.join(format!("v1-{key:016x}.mkv"));
                file.symlink_metadata()
                    .is_ok_and(|m| m.is_file() && m.len() > 0)
                    .then_some(file)
            })
            .flatten();
        Some(Self {
            source,
            output,
            stamp,
            hdr,
            av1,
            cached,
            legacy,
        })
    }

    pub fn still_matches(&self) -> bool {
        source_stamp(&self.source) == Some(self.stamp)
            && prepared_marker(&self.output) == Some(self.stamp)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Gpu,
    Cpu,
    Reuse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: f64,
    pub total: f64,
    speed: Option<f64>,
}

impl Progress {
    pub fn fraction(self) -> Option<f64> {
        (self.total > 0.0).then(|| (self.done / self.total).clamp(0.0, 1.0))
    }

    pub fn eta(self) -> Option<f64> {
        let speed = self.speed?;
        (self.done > 0.25 && self.total > 0.0)
            .then(|| ((self.total - self.done).max(0.0) / speed).ceil())
    }
}

#[derive(Clone, Debug)]
pub enum Status {
    Running(Method, Progress),
    Ready,
    Cancelled,
    Failed(String),
}

pub struct Job {
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Job {
    pub fn start(target: Target, total: f64, vaapi: Option<PathBuf>) -> Result<Self, String> {
        let method = if target.legacy.is_some() {
            Method::Reuse
        } else if vaapi.is_some() {
            Method::Gpu
        } else {
            Method::Cpu
        };
        let status = Arc::new(Mutex::new(Status::Running(method, Progress::default())));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_status = Arc::clone(&status);
        let worker_cancel = Arc::clone(&cancel);
        let worker = thread::Builder::new()
            .name("coffee-prepare".into())
            .spawn(move || {
                let result = prepare(
                    &target,
                    total,
                    vaapi.as_deref(),
                    &worker_status,
                    &worker_cancel,
                );
                *worker_status.lock().unwrap() = match result {
                    Ok(()) => Status::Ready,
                    Err(_) if worker_cancel.load(Ordering::Relaxed) => Status::Cancelled,
                    Err(error) => Status::Failed(error),
                };
            })
            .map_err(|e| format!("Couldn’t start preparation: {e}"))?;
        Ok(Self {
            status,
            cancel,
            worker: Some(worker),
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

const MAX_SIZE: &str =
    "w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2";
const HDR_CONVERT: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=hable,zscale=t=bt709:m=bt709:r=limited";

fn ffmpeg() -> Command {
    let program = env::var_os("COFFEE_FFMPEG").map(PathBuf::from);
    #[cfg(target_os = "windows")]
    let program = program.or_else(|| {
        let msys = env::var_os("MSYS2_ROOT").unwrap_or_else(|| r"C:\msys64".into());
        let path = PathBuf::from(msys).join(r"ucrt64\bin\ffmpeg.exe");
        path.is_file().then_some(path)
    });
    #[cfg(target_os = "macos")]
    let program = program.or_else(|| {
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    });
    let cmd = Command::new(program.unwrap_or_else(|| "ffmpeg".into()));
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        cmd
    };
    cmd
}

fn progress_command() -> Command {
    let mut cmd = ffmpeg();
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostats",
        "-y",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.5",
        "-filter_threads",
        "2",
    ]);
    cmd
}

fn video_only(cmd: &mut Command, source: &Path) {
    cmd.arg("-i").arg(source).args([
        "-map",
        "0:V:0",
        "-an",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
    ]);
}

fn mp4(cmd: &mut Command, output: &Path) {
    cmd.args(["-movflags", "+faststart", "-f", "mp4"])
        .arg(output);
}

fn command(target: &Target, output: &Path, vaapi: Option<&Path>) -> Command {
    let mut cmd = progress_command();
    if let Some(device) = vaapi {
        cmd.arg("-vaapi_device").arg(device);
    }
    let threads = thread::available_parallelism()
        .map_or(4, usize::from)
        .min(8);
    cmd.args(["-threads", &threads.to_string()]);
    // Prefer the faster AV1 software decoder when present. FFmpeg builds
    // without dav1d are retried with their default AV1 decoder.
    if target.av1 {
        cmd.args(["-c:v", "libdav1d"]);
    }
    video_only(&mut cmd, &target.source);
    cmd.args(["-fps_mode", "passthrough"]);
    if vaapi.is_some() {
        let vf = format!(
            "libplacebo={MAX_SIZE}:format=nv12:colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv:tonemapping=hable:peak_detect=0:downscaler=bilinear:sigmoid=0,format=nv12,hwupload"
        );
        cmd.args(["-vf", &vf, "-c:v", "h264_vaapi", "-qp", "22"]);
    } else {
        let vf = if target.hdr {
            format!("scale={MAX_SIZE},{HDR_CONVERT},format=yuv420p")
        } else {
            format!("scale={MAX_SIZE},format=yuv420p")
        };
        cmd.args([
            "-vf", &vf, "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-threads", "2",
        ]);
    }
    mp4(&mut cmd, output);
    cmd
}

fn prepare(
    target: &Target,
    total: f64,
    vaapi: Option<&Path>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if source_stamp(&target.source) != Some(target.stamp) {
        return Err("The original video changed. Reopen it to prepare this version.".into());
    }
    let dir = target
        .output
        .parent()
        .ok_or("No folder for the prepared video.")?;
    let temp = tempfile::Builder::new()
        .prefix(".coffee-preparing-")
        .suffix(".mp4")
        .tempfile_in(dir)
        .map_err(|e| format!("Couldn’t write next to the original video: {e}"))?
        .into_temp_path();
    let reused = if let Some(legacy) = &target.legacy {
        let mut cmd = progress_command();
        video_only(&mut cmd, legacy);
        cmd.args(["-c:v", "copy"]);
        mp4(&mut cmd, &temp);
        match convert(cmd, total, Method::Reuse, status, cancel) {
            Ok(()) => true,
            Err(e) if cancel.load(Ordering::Relaxed) || e.starts_with("Install FFmpeg") => {
                return Err(e);
            }
            Err(_) => false, // The legacy cache is optional; encode the source if unreadable.
        }
    } else {
        false
    };
    if !reused {
        let mut variant = target.clone();
        let mut device = vaapi;
        loop {
            let method = if device.is_some() {
                Method::Gpu
            } else {
                Method::Cpu
            };
            let result = convert(
                command(&variant, &temp, device),
                total,
                method,
                status,
                cancel,
            );
            match result {
                Ok(()) => break,
                Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
                Err(e) if variant.av1 && e.contains("Unknown decoder") => variant.av1 = false,
                Err(e) if device.is_some() && !e.starts_with("Install FFmpeg") => device = None,
                Err(e) => return Err(e),
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    if source_stamp(&target.source) != Some(target.stamp) {
        return Err("The original video changed during preparation.".into());
    }
    stamp_output(&temp, target.stamp)
        .map_err(|e| format!("Couldn’t finish the prepared MP4: {e}"))?;
    match temp.persist_noclobber(&target.output) {
        Ok(()) => Ok(()),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
            if prepared_marker(&target.output) == Some(target.stamp) {
                Ok(())
            } else {
                Err(
                    "That output name was taken. Reopen the original to choose a different name."
                        .into(),
                )
            }
        }
        Err(e) => Err(format!("Couldn’t save the prepared copy: {}", e.error)),
    }
}

fn convert(
    mut command: Command,
    total: f64,
    method: Method,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    let mut progress = Progress {
        total,
        ..Progress::default()
    };
    *status.lock().unwrap() = Status::Running(method, progress);
    let mut errors: File = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(errors.try_clone().map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                "Install FFmpeg to prepare video.".into()
            } else {
                format!("Couldn’t start FFmpeg: {e}")
            }
        })?;
    let stdout = child.stdout.take().unwrap();
    let result = thread::scope(|scope| {
        scope.spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if update_progress(&mut progress, &line) {
                    *status.lock().unwrap() = Status::Running(method, progress);
                }
            }
        });
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Cancelled".into());
            }
            match child.try_wait() {
                Ok(Some(code)) => return Ok(code),
                Ok(None) => thread::sleep(Duration::from_millis(75)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Couldn’t wait for FFmpeg: {e}"));
                }
            }
        }
    })?;
    if result.success() {
        return Ok(());
    }
    let _ = errors.rewind();
    let mut details = Vec::new();
    let _ = errors.take(4096).read_to_end(&mut details);
    let details = String::from_utf8_lossy(&details);
    let details = details
        .lines()
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    Err(if details.is_empty() {
        format!("FFmpeg stopped ({result}).")
    } else {
        format!("FFmpeg: {details}")
    })
}

fn update_progress(progress: &mut Progress, line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    match key {
        "out_time_us" => {
            if let Ok(us) = value.parse::<i64>() {
                progress.done = progress.done.max(us as f64 / 1_000_000.0);
            }
        }
        "speed" => {
            if let Some(speed) = value
                .trim()
                .strip_suffix('x')
                .and_then(|s| s.parse::<f64>().ok())
                && speed.is_finite()
                && speed > 0.0
            {
                progress.speed = Some(speed);
            }
        }
        "progress" => return true,
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_needs_sustained_drops_and_ignores_a_seek() {
        let t = Instant::now();
        let mut health = PlaybackHealth::default();
        for (s, position, drops) in [(0, 0.0, 0), (1, 1.0, 9), (2, 41.0, 9), (3, 42.0, 17)] {
            health.observe(t + Duration::from_secs(s), position, 1.0, 60.0, drops);
            assert!(!health.detected());
        }
        health.observe(t + Duration::from_secs(4), 43.0, 1.0, 60.0, 27);
        assert!(health.detected());
        health.interrupt();
        assert!(health.detected()); // Stay available once offered for this item.
    }

    #[test]
    fn health_can_recognize_stalled_playback_without_drops() {
        let mut health = PlaybackHealth::default();
        let t = Instant::now();
        for (second, position) in [(0, 1.0), (1, 1.2), (2, 1.5)] {
            health.observe(t + Duration::from_secs(second), position, 1.0, 24.0, 0);
        }
        assert!(health.detected());
    }

    #[test]
    fn ffmpeg_eta_uses_media_time_and_reported_wall_clock_speed() {
        let mut p = Progress {
            total: 160.0,
            ..Progress::default()
        };
        assert_eq!(p.eta(), None);
        assert!(!update_progress(&mut p, "out_time_us=40000000"));
        assert!(!update_progress(&mut p, "speed= 0.4x"));
        assert!(update_progress(&mut p, "progress=continue"));
        assert_eq!(p.fraction(), Some(0.25));
        assert_eq!(p.eta(), Some(300.0));
        update_progress(&mut p, "out_time_us=N/A");
        update_progress(&mut p, "speed=0x");
        assert_eq!(p.eta(), Some(300.0));
    }

    #[test]
    fn preparation_fingerprint_changes_with_file_size() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        assert_eq!(fingerprint(&path), None);
        file.write_all(b"original").unwrap();
        let first = fingerprint(&path).unwrap();
        file.write_all(b"changed").unwrap();
        assert_ne!(fingerprint(&path), Some(first));
    }

    fn fake_mp4(path: &Path, stamp: u64) {
        fs::write(path, b"\x00\x00\x00\x10ftypisom0000").unwrap();
        stamp_output(path, stamp).unwrap();
    }

    #[test]
    fn every_source_uses_its_own_stem_and_the_same_folder() {
        let temp = tempfile::tempdir().unwrap();
        for (input, output) in [
            ("sample.mp4", "sample-coffee.mp4"),
            ("Holiday.mov", "Holiday-coffee.mp4"),
            ("Episode 2.1080p.webm", "Episode 2.1080p-coffee.mp4"),
            ("فیلم.mkv", "فیلم-coffee.mp4"),
        ] {
            let source = temp.path().join(input);
            fs::write(&source, b"source").unwrap();
            let t = Target::local(source.to_str().unwrap(), false, false).unwrap();
            assert_eq!(t.output, temp.path().join(output));
            assert!(!t.cached);
        }
    }

    #[test]
    fn sibling_reuse_skips_unrelated_files_and_never_reuses_a_stale_copy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("a.webm");
        let first = temp.path().join("a-coffee.mp4");
        let second = temp.path().join("a-coffee-2.mp4");
        fs::write(&source, b"source").unwrap();
        fs::write(&first, b"unrelated file").unwrap();
        let key = source_stamp(&source).unwrap();
        assert_eq!(sibling_output(&source, key), Some((second.clone(), false)));
        fake_mp4(&second, key);
        assert_eq!(sibling_output(&source, key), Some((second.clone(), true)));
        assert_eq!(fs::read(&first).unwrap(), b"unrelated file");
        fs::remove_file(&first).unwrap();
        assert_eq!(sibling_output(&source, key), Some((second.clone(), true)));
        let copy = Target::local(second.to_str().unwrap(), false, false);
        assert!(copy.is_none(), "prepared files do not get prepared again");

        fs::write(&source, b"replacement content").unwrap();
        let changed = Target::local(source.to_str().unwrap(), false, false).unwrap();
        assert!(!changed.cached);
        assert_eq!(changed.output, first);
        assert_eq!(prepared_marker(&second), Some(key));
    }

    #[test]
    fn stamped_copy_still_matches_after_moving_its_folder() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let to = root.path().join("to");
        fs::create_dir(&from).unwrap();
        let source = from.join("track.avi");
        fs::write(&source, b"source").unwrap();
        let target = Target::local(source.to_str().unwrap(), false, false).unwrap();
        fake_mp4(&target.output, target.stamp);
        assert!(target.still_matches());
        fs::rename(from, &to).unwrap();
        let moved = Target::local(to.join("track.avi").to_str().unwrap(), false, false).unwrap();
        assert!(moved.cached && moved.still_matches());
    }
}
