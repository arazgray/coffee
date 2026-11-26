//! A small in-memory timeline index and one coalesced request for cold areas.
//! Neither FFmpeg process seeks or blocks the main player.
use crate::{engine::Engine, prepare};
use std::{
    collections::VecDeque,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SIZE: (usize, usize) = (192, 108);
const BYTES: usize = SIZE.0 * SIZE.1 * 4;
const INDEX_LIMIT: usize = 320;
const DEMAND_LIMIT: usize = 24;
const OVERLAY: &str = "3";
type Bounds = (i32, i32, i32, i32);

#[derive(Clone, Copy)]
struct Plan {
    interval: f64,
    count: usize,
}

impl Plan {
    fn new(duration: f64) -> Option<Self> {
        if !duration.is_finite() || duration <= 0.0 {
            return None;
        }
        // Half-second resolution for short videos; keep the whole index
        // under 26 MiB even for a feature-length film.
        let interval = ((duration / (INDEX_LIMIT - 1) as f64) * 2.0)
            .ceil()
            .max(1.0)
            / 2.0;
        let count = (duration / interval).ceil().clamp(1.0, INDEX_LIMIT as f64) as usize;
        Some(Self { interval, count })
    }
}

struct Index {
    plan: Plan,
    frames: Vec<Vec<u8>>,
    finished: bool,
}

impl Index {
    fn near(&self, seconds: f64) -> Option<usize> {
        let last = self.frames.len().checked_sub(1)?;
        let frame = (seconds / self.plan.interval).round() as usize;
        let frame = frame.min(last);
        ((seconds - frame as f64 * self.plan.interval).abs() <= self.plan.interval * 1.25)
            .then_some(frame)
    }
}

enum IndexEvent {
    Frame(usize, Vec<u8>),
    Finished(Outcome),
}

#[derive(PartialEq, Eq)]
enum Outcome {
    Done,
    Failed,
    Unavailable,
    Cancelled,
}

struct Worker<T> {
    cancel: Arc<AtomicBool>,
    result: Receiver<T>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(
        name: &str,
        run: impl FnOnce(&AtomicBool, Sender<T>) + Send + 'static,
    ) -> Option<Self> {
        let (tx, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || run(&token, tx))
            .ok()?;
        Some(Self {
            cancel,
            result,
            thread: Some(thread),
        })
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Demand = Worker<(Outcome, Option<Vec<u8>>)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Image {
    Index(usize),
    Demand(u64),
}

#[derive(Default)]
pub struct Preview {
    source: Option<(PathBuf, bool)>,
    generation: u64,
    index: Option<Index>,
    indexing: Option<(u64, Worker<IndexEvent>)>,
    demand: Option<(u64, u64, Demand)>,
    requested: VecDeque<(u64, Option<Vec<u8>>)>,
    drawn: Option<(Image, Bounds)>,
    hovering: bool,
    unavailable: bool,
}

impl Preview {
    pub fn source(&mut self, mpv: &Engine, source: Option<(PathBuf, bool)>) {
        self.source = source;
        self.generation = self.generation.wrapping_add(1);
        self.index = None;
        self.requested.clear();
        self.hovering = false;
        self.cancel_jobs();
        self.hide(mpv);
    }

    pub fn hovering(&self) -> bool {
        self.hovering
    }

    fn cancel_jobs(&self) {
        if let Some((_, work)) = &self.indexing {
            work.cancel();
        }
        if let Some((_, _, work)) = &self.demand {
            work.cancel();
        }
    }

    fn hide(&mut self, mpv: &Engine) {
        if self.drawn.take().is_some() {
            let _ = mpv.command(&["overlay-remove", OVERLAY]);
        }
    }

    fn collect(&mut self) {
        if let Some((generation, work)) = &self.indexing {
            let current = *generation == self.generation;
            for _ in 0..12 {
                match work.result.try_recv() {
                    Ok(IndexEvent::Frame(i, frame)) => {
                        if current
                            && let Some(index) = &mut self.index
                            && i == index.frames.len()
                            && i < index.plan.count
                        {
                            index.frames.push(frame);
                        }
                    }
                    Ok(IndexEvent::Finished(status)) => {
                        self.unavailable |= status == Outcome::Unavailable;
                        if current && let Some(index) = &mut self.index {
                            index.finished = status != Outcome::Cancelled;
                        }
                        self.indexing = None;
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        if current && let Some(index) = &mut self.index {
                            index.finished = true;
                        }
                        self.indexing = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        if let Some((generation, key, work)) = &self.demand {
            match work.result.try_recv() {
                Ok((status, pixels)) => {
                    let current = *generation == self.generation;
                    let key = *key;
                    self.demand = None;
                    self.unavailable |= status == Outcome::Unavailable;
                    if current && matches!(status, Outcome::Done | Outcome::Failed) {
                        if self.requested.len() == DEMAND_LIMIT {
                            self.requested.pop_front();
                        }
                        self.requested.push_back((key, pixels));
                    }
                }
                Err(TryRecvError::Disconnected) => self.demand = None,
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn start_index(&mut self, duration: f64) {
        if self.indexing.is_some()
            || self.unavailable
            || self.index.as_ref().is_some_and(|index| index.finished)
        {
            return;
        }
        let Some((source, hdr)) = self.source.clone() else {
            return;
        };
        let Some(plan) = Plan::new(duration) else {
            return;
        };
        let index = self.index.get_or_insert_with(|| Index {
            plan,
            frames: vec![],
            finished: false,
        });
        let plan = index.plan;
        // Resume without evicting thumbnails already shown if Smooth
        // Playback temporarily reserved the decoder.
        if let Some(work) = Worker::start("coffee-thumbnail-index", move |cancel, tx| {
            // The last keyframe can be several seconds before EOF. Hold
            // that preview through the tail; the frame limit stops tpad.
            let filter = format!(
                "fps=1/{:.3}:start_time=0:round=near,tpad=stop_mode=clone:stop=-1,{}",
                plan.interval,
                resize_filter(hdr)
            );
            let cmd = command(&source, &["-skip_frame", "nokey"], &filter, plan.count);
            let mut slot = 0;
            let outcome = read_frames(cmd, plan.count, Duration::from_secs(25), cancel, |pixels| {
                let _ = tx.send(IndexEvent::Frame(slot, pixels));
                slot += 1;
            });
            let _ = tx.send(IndexEvent::Finished(outcome));
        }) {
            self.indexing = Some((self.generation, work));
        } else {
            index.finished = true;
        }
    }

    fn start_demand(&mut self, key: u64) {
        if self.demand.is_some() || self.requested.iter().any(|(t, _)| *t == key) {
            return;
        }
        let Some((source, hdr)) = self.source.clone() else {
            return;
        };
        if let Some(work) = Worker::start("coffee-thumbnail-demand", move |cancel, tx| {
            let seconds = format!("{:.3}", key as f64 / 2.0);
            let cmd = command(
                &source,
                &["-noaccurate_seek", "-ss", &seconds],
                &resize_filter(hdr),
                1,
            );
            let mut pixels = None;
            let outcome = read_frames(cmd, 1, Duration::from_secs(3), cancel, |frame| {
                pixels = Some(frame)
            });
            let _ = tx.send((outcome, pixels));
        }) {
            self.demand = Some((self.generation, key, work));
        }
    }

    fn pixels(&self, image: Image) -> Option<&[u8]> {
        match image {
            Image::Index(i) => Some(self.index.as_ref()?.frames.get(i)?.as_slice()),
            Image::Demand(t) => self
                .requested
                .iter()
                .find(|(key, _)| *key == t)?
                .1
                .as_deref(),
        }
    }

    pub fn update(
        &mut self,
        mpv: &Engine,
        request: Option<(f64, Bounds)>,
        duration: f64,
        allowed: bool,
    ) -> bool {
        self.collect();
        if allowed {
            self.start_index(duration);
        } else {
            self.cancel_jobs();
        }
        let request = request.filter(|_| self.source.is_some() && allowed && !self.unavailable);
        self.hovering = request.is_some();
        let Some((seconds, bounds)) = request else {
            if let Some((_, _, work)) = &self.demand {
                work.cancel();
            }
            self.hide(mpv);
            return false;
        };

        let key = bucket(seconds);
        let indexed = self
            .index
            .as_ref()
            .and_then(|index| index.near(seconds))
            .map(Image::Index);
        let exact = self.pixels(Image::Demand(key)).map(|_| Image::Demand(key));
        if indexed.is_none() && exact.is_none() {
            // Coalesce to the latest cursor position once the current seek
            // completes. Don't cancel on movement: that starves fast scrubs.
            self.start_demand(key);
        }
        let nearby = self
            .requested
            .iter()
            .filter(|(t, pixels)| pixels.is_some() && t.abs_diff(key) <= 4)
            .min_by_key(|(t, _)| t.abs_diff(key))
            .map(|(t, _)| Image::Demand(*t));
        let image = indexed
            .or(exact)
            .or(nearby)
            .or(self.drawn.map(|(image, _)| image));
        if let Some(image) = image
            && let Some(pixels) = self.pixels(image)
        {
            if self.drawn == Some((image, bounds)) {
                return true;
            }
            if mpv.bitmap_overlay(OVERLAY, SIZE, pixels, bounds) {
                self.drawn = Some((image, bounds));
                return true;
            }
        }
        self.hide(mpv);
        false
    }
}

fn bucket(seconds: f64) -> u64 {
    (seconds.max(0.0) * 2.0).floor() as u64
}

fn resize_filter(hdr: bool) -> String {
    let scale = "scale=192:108:force_original_aspect_ratio=decrease:force_divisible_by=2";
    let finish = "format=bgra,pad=192:108:(ow-iw)/2:(oh-ih)/2,setsar=1";
    if hdr {
        format!("{scale},{},{finish}", prepare::HDR_CONVERT)
    } else {
        format!("{scale},{finish}")
    }
}

fn command(path: &Path, input: &[&str], filter: &str, limit: usize) -> Command {
    let mut cmd = prepare::ffmpeg();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS.
        cmd.creation_flags(0x0800_0000 | 0x0000_4000);
    }
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file,crypto,data",
        "-threads",
        "1",
        "-filter_threads",
        "1",
    ])
    .args(input)
    .arg("-i")
    .arg(path)
    .args([
        "-map",
        "0:V:0",
        "-an",
        "-sn",
        "-dn",
        "-vf",
        filter,
        "-frames:v",
        &limit.to_string(),
        "-threads",
        "1",
        "-fps_mode",
        "passthrough",
        "-pix_fmt",
        "bgra",
        "-c:v",
        "rawvideo",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    cmd
}

fn read_frames(
    mut command: Command,
    limit: usize,
    timeout: Duration,
    cancel: &AtomicBool,
    mut on_frame: impl FnMut(Vec<u8>) + Send,
) -> Outcome {
    if cancel.load(Ordering::Relaxed) {
        return Outcome::Cancelled;
    }
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::Unavailable,
        Err(_) => return Outcome::Failed,
    };
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, priority: i32) -> i32;
        }
        // Prefer normal video and audio decoding over thumbnail work.
        let _ = unsafe { setpriority(0, child.id(), 8) };
    }
    let mut stdout = child.stdout.take().unwrap();
    let count = AtomicUsize::new(0);
    thread::scope(|scope| {
        let produced = &count;
        scope.spawn(move || {
            for i in 0..limit {
                let mut pixels = vec![0; BYTES];
                if stdout.read_exact(&mut pixels).is_err() {
                    break;
                }
                on_frame(pixels);
                produced.store(i + 1, Ordering::Relaxed);
            }
        });
        let mut last = Instant::now();
        let mut seen = 0;
        loop {
            let n = count.load(Ordering::Relaxed);
            if n != seen {
                seen = n;
                last = Instant::now();
            }
            if cancel.load(Ordering::Relaxed) || last.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return if cancel.load(Ordering::Relaxed) {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                };
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Outcome::Done
                    } else {
                        Outcome::Failed
                    };
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Outcome::Failed;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_samples_are_dense_for_short_videos_and_bounded_for_long_ones() {
        let short = Plan::new(158.0).unwrap();
        assert_eq!(short.interval, 0.5);
        assert_eq!(short.count, 316);
        let long = Plan::new(7200.0).unwrap();
        assert!(long.interval > 20.0 && long.count <= INDEX_LIMIT);
        assert!(Plan::new(f64::NAN).is_none());
    }

    #[test]
    fn a_progressive_index_selects_nearest_frames_without_using_distant_entries() {
        let plan = Plan::new(10.0).unwrap();
        let index = Index {
            plan,
            frames: vec![vec![]; 5],
            finished: false,
        };
        assert_eq!(index.near(0.28), Some(1));
        assert_eq!(index.near(1.46), Some(3));
        assert_eq!(index.near(2.20), Some(4));
        assert_eq!(index.near(8.0), None);
    }

    #[test]
    fn a_cold_preview_uses_half_second_keys() {
        assert_eq!(bucket(40.01), bucket(40.49));
        assert_ne!(bucket(40.01), bucket(40.50));
    }
}
