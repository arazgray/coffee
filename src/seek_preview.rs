//! Debounced, bounded thumbnail decoding. No seeks are issued to the player.
use crate::{engine::Engine, prepare};
use std::{
    collections::VecDeque,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SIZE: (usize, usize) = (192, 108);
const BYTES: usize = SIZE.0 * SIZE.1 * 4;
const DEBOUNCE: Duration = Duration::from_millis(170);
const TIMEOUT: Duration = Duration::from_secs(4);
const CAPACITY: usize = 24;
const OVERLAY: &str = "3";
type Bounds = (i32, i32, i32, i32);

enum ResultFrame {
    Pixels(Vec<u8>),
    Failed,
    Unavailable,
    Cancelled,
}

struct Work {
    generation: u64,
    key: u64,
    cancel: Arc<AtomicBool>,
    result: Receiver<ResultFrame>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Work {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub struct Preview {
    source: Option<(PathBuf, bool)>,
    generation: u64,
    hover: Option<(u64, Instant)>,
    work: Option<Work>,
    cache: VecDeque<(u64, Option<Vec<u8>>)>,
    drawn: Option<(u64, Bounds)>,
    unavailable: bool,
}

impl Preview {
    pub fn source(&mut self, mpv: &Engine, source: Option<(PathBuf, bool)>) {
        self.source = source;
        self.generation = self.generation.wrapping_add(1);
        self.cache.clear();
        self.hover = None;
        if let Some(work) = &self.work {
            work.cancel.store(true, Ordering::Relaxed);
        }
        self.hide(mpv);
    }

    fn hide(&mut self, mpv: &Engine) {
        if self.drawn.take().is_some() {
            let _ = mpv.command(&["overlay-remove", OVERLAY]);
        }
    }

    pub fn update(&mut self, mpv: &Engine, request: Option<(f64, Bounds)>, now: Instant) -> bool {
        if let Some(work) = &self.work {
            match work.result.try_recv() {
                Ok(result) => {
                    let current = work.generation == self.generation;
                    let key = work.key;
                    self.work = None;
                    let image = match result {
                        ResultFrame::Pixels(pixels) if current => Some(Some(pixels)),
                        ResultFrame::Failed if current => Some(None),
                        ResultFrame::Unavailable => {
                            self.unavailable = true;
                            None
                        }
                        _ => None,
                    };
                    if let Some(image) = image {
                        if self.cache.len() >= CAPACITY {
                            self.cache.pop_front();
                        }
                        self.cache.push_back((key, image));
                    }
                }
                Err(TryRecvError::Disconnected) => self.work = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let request = request.filter(|_| self.source.is_some() && !self.unavailable);
        let requested_key = request.map(|(seconds, _)| bucket(seconds));
        if let Some(work) = &self.work
            && (Some(work.key) != requested_key || work.generation != self.generation)
        {
            work.cancel.store(true, Ordering::Relaxed);
        }
        let Some((seconds, bounds)) = request else {
            self.hover = None;
            self.hide(mpv);
            return false;
        };
        let key = bucket(seconds);
        if self.hover.is_none_or(|(old, _)| old != key) {
            self.hover = Some((key, now));
        }
        if let Some((_, image)) = self.cache.iter().find(|(cached, _)| *cached == key) {
            if let Some(image) = image {
                if self.drawn == Some((key, bounds)) {
                    return true;
                }
                if mpv.bitmap_overlay(OVERLAY, SIZE, image, bounds) {
                    self.drawn = Some((key, bounds));
                    return true;
                }
            }
        } else if self.work.is_none()
            && self.hover.is_some_and(|(_, since)| now - since >= DEBOUNCE)
            && let Some((source, hdr)) = self.source.clone()
        {
            let (tx, result) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let request_cancel = Arc::clone(&cancel);
            if let Ok(thread) = thread::Builder::new()
                .name("coffee-thumbnail".into())
                .spawn(move || {
                    let _ = tx.send(decode(&source, hdr, key as f64 / 2.0, &request_cancel));
                })
            {
                self.work = Some(Work {
                    generation: self.generation,
                    key,
                    cancel,
                    result,
                    thread: Some(thread),
                });
            }
        }
        self.hide(mpv);
        false
    }
}

fn bucket(seconds: f64) -> u64 {
    (seconds.max(0.0) * 2.0).floor() as u64
}

fn decode(path: &Path, hdr: bool, seconds: f64, cancel: &AtomicBool) -> ResultFrame {
    if cancel.load(Ordering::Relaxed) {
        return ResultFrame::Cancelled;
    }
    let scale = "scale=192:108:force_original_aspect_ratio=decrease:force_divisible_by=2";
    let finish = "format=bgra,pad=192:108:(ow-iw)/2:(oh-ih)/2,setsar=1";
    let filter = if hdr {
        format!("{scale},{},{finish}", prepare::HDR_CONVERT)
    } else {
        format!("{scale},{finish}")
    };
    let mut cmd = prepare::ffmpeg();
    let mut child = match cmd
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "2",
            "-filter_threads",
            "1",
            "-ss",
            &format!("{seconds:.3}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:V:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-pix_fmt",
            "bgra",
            "-c:v",
            "rawvideo",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ResultFrame::Unavailable,
        Err(_) => return ResultFrame::Failed,
    };
    let stdout = child.stdout.take().unwrap();
    thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut pixels = Vec::with_capacity(BYTES);
            stdout
                .take((BYTES + 1) as u64)
                .read_to_end(&mut pixels)
                .ok()?;
            (pixels.len() == BYTES).then_some(pixels)
        });
        let start = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) || start.elapsed() >= TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return if cancel.load(Ordering::Relaxed) {
                    ResultFrame::Cancelled
                } else {
                    ResultFrame::Failed
                };
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    return match reader.join() {
                        Ok(Some(pixels)) => ResultFrame::Pixels(pixels),
                        _ => ResultFrame::Failed,
                    };
                }
                Ok(Some(_)) => return ResultFrame::Failed,
                Ok(None) => thread::sleep(Duration::from_millis(15)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ResultFrame::Failed;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moving_within_a_half_second_reuses_the_thumbnail() {
        assert_eq!(bucket(40.01), bucket(40.49));
        assert_ne!(bucket(40.01), bucket(40.50));
        assert_eq!(bucket(-1.0), 0);
    }
}
