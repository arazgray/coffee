//! Cancellable FFmpeg jobs. The cache holds video-only alternatives so mpv
//! can keep the original audio, subtitles, chapters and playlist entry.
use std::{
    env,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Seek},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone)]
pub struct Target {
    source: PathBuf,
    pub output: PathBuf,
    hdr: bool,
    av1: bool,
    pub cached: bool,
}

fn cache_directory() -> Option<PathBuf> {
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

impl Target {
    pub fn local(path: &str, hdr: bool, av1: bool) -> Option<Self> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return None;
        }
        let source = path.canonicalize().ok()?;
        let key = fingerprint(&source)?;
        let output = cache_directory()?.join(format!("v1-{key:016x}.mkv"));
        let cached = output.metadata().is_ok_and(|m| m.is_file() && m.len() > 0);
        Some(Self {
            source,
            output,
            hdr,
            av1,
            cached,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Gpu,
    Cpu,
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
        let method = if vaapi.is_some() {
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

fn command(target: &Target, output: &Path, vaapi: Option<&Path>) -> Command {
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
    cmd.arg("-i").arg(&target.source).args([
        "-map",
        "0:V:0",
        "-an",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-fps_mode",
        "passthrough",
    ]);
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
    cmd.args(["-f", "matroska"]).arg(output);
    cmd
}

fn prepare(
    target: &Target,
    total: f64,
    vaapi: Option<&Path>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let original = fingerprint(&target.source).ok_or("The original video is unavailable.")?;
    let dir = target.output.parent().ok_or("No cache directory.")?;
    fs::create_dir_all(dir)
        .map_err(|e| format!("Couldn’t create the prepared-video cache: {e}"))?;
    let temp = tempfile::Builder::new()
        .prefix(".preparing-")
        .suffix(".mkv")
        .tempfile_in(dir)
        .map_err(|e| format!("Couldn’t create a prepared video: {e}"))?
        .into_temp_path();
    let mut variant = target.clone();
    let mut device = vaapi;
    loop {
        let result = convert(&variant, &temp, total, device, status, cancel);
        match result {
            Ok(()) => break,
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) if variant.av1 && e.contains("Unknown decoder") => variant.av1 = false,
            Err(e) if device.is_some() && !e.starts_with("Install FFmpeg") => device = None,
            Err(e) => return Err(e),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    if fingerprint(&target.source) != Some(original) {
        return Err("The original video changed during preparation.".into());
    }
    if !temp.metadata().is_ok_and(|meta| meta.len() > 0) {
        return Err("FFmpeg did not produce a video.".into());
    }
    match temp.persist_noclobber(&target.output) {
        Ok(()) => Ok(()),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists && target.output.is_file() => {
            Ok(())
        }
        Err(e) => Err(format!("Couldn’t save the prepared copy: {}", e.error)),
    }
}

fn convert(
    target: &Target,
    output: &Path,
    total: f64,
    vaapi: Option<&Path>,
    status: &Mutex<Status>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    let method = if vaapi.is_some() {
        Method::Gpu
    } else {
        Method::Cpu
    };
    let mut progress = Progress {
        total,
        ..Progress::default()
    };
    *status.lock().unwrap() = Status::Running(method, progress);
    let mut errors: File = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = command(target, output, vaapi)
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
        use std::io::Write;
        file.write_all(b"original").unwrap();
        let first = fingerprint(&path).unwrap();
        file.write_all(b"changed").unwrap();
        assert_ne!(fingerprint(&path), Some(first));
    }
}
