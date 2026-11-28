//! One playback owner per user session, with independent photo processes.
use crate::{library, state::image_path};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_REQUEST: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Mode {
    #[default]
    Play,
    Add,
    Replace(usize),
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Request {
    pub files: Vec<String>,
    pub mode: Mode,
}

impl Request {
    pub fn split(paths: impl IntoIterator<Item = String>, mode: Mode) -> (Self, Vec<String>) {
        let mut playback = Self {
            files: vec![],
            mode,
        };
        let mut photos = Vec::new();
        let mut start = 0;
        for (index, path) in paths.into_iter().enumerate() {
            let Some(path) = library::normalize(&path) else {
                continue;
            };
            if image_path(&path) {
                photos.push(path);
            } else {
                if matches!(mode, Mode::Replace(current) if index <= current) {
                    start = playback.files.len();
                }
                playback.files.push(path);
            }
        }
        if matches!(mode, Mode::Replace(_)) {
            playback.mode = Mode::Replace(start);
        }
        (playback, photos)
    }

    pub fn spawn(&self) -> io::Result<()> {
        let mut command = coffee()?;
        match self.mode {
            Mode::Play => {}
            Mode::Add => {
                command.arg("--add-to-queue");
            }
            Mode::Replace(index) => {
                command.args(["--replace-queue", &index.to_string()]);
            }
        }
        spawn(command.args(&self.files))
    }
}

pub fn open_photo(path: &str) -> io::Result<()> {
    spawn(coffee()?.arg(path))
}

fn coffee() -> io::Result<Command> {
    #[allow(unused_mut)]
    let mut command = Command::new(env::current_exe()?);
    #[cfg(target_os = "macos")]
    command.env("MPVBUNDLE", "true");
    Ok(command)
}

fn spawn(command: &mut Command) -> io::Result<()> {
    let mut child = command.spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn runtime_dir() -> io::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))
            }
        });
    let root = base
        .ok_or_else(|| io::Error::other("Coffee needs a user runtime directory"))?
        .join("coffee");
    private_dir(&root)?;
    Ok(root)
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut dir = fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dir.mode(0o700);
    }
    dir.create(path)
}

pub struct Playback {
    // The OS releases this lock on exit or crash. Never unlink the lock
    // file, since a concurrent launch could still have it open.
    _owner: File,
    inbox: PathBuf,
}

impl Playback {
    /// Claim playback or send an atomic message to the existing owner.
    /// A sender can take over if the player closes before reading its message.
    pub fn open(root: &Path, request: &Request) -> io::Result<Option<Self>> {
        let inbox = root.join("open-requests");
        private_dir(&inbox)?;
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("playback.lock"))?;
        if try_claim(&owner)? {
            return Ok(Some(Self {
                _owner: owner,
                inbox,
            }));
        }
        let mut temp = tempfile::Builder::new()
            .prefix(".pending-")
            .tempfile_in(&inbox)?;
        let bytes = serde_json::to_vec(request)?;
        if bytes.len() as u64 > MAX_REQUEST {
            return Err(io::Error::other("That open request is larger than 2 MiB"));
        }
        temp.write_all(&bytes)?;
        temp.flush()?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = inbox.join(format!(
            "{stamp:030}-{}.request",
            temp.path().file_name().unwrap().to_string_lossy()
        ));
        drop(temp.persist_noclobber(&path)?);

        let deadline = Instant::now() + Duration::from_millis(600);
        while path.try_exists()? {
            if try_claim(&owner)? {
                match fs::remove_file(&path) {
                    Ok(()) => {
                        return Ok(Some(Self {
                            _owner: owner,
                            inbox,
                        }));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
            if Instant::now() >= deadline {
                // The existing player may still be loading. The request is
                // on disk and will be read when its event loop is ready.
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(None)
    }

    pub fn pending(&self) -> io::Result<Vec<Request>> {
        let mut paths = fs::read_dir(&self.inbox)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "request"))
            .collect::<Vec<_>>();
        paths.sort_unstable();
        let mut requests = Vec::new();
        for path in paths {
            let mut data = Vec::new();
            if File::open(&path)
                .and_then(|file| file.take(MAX_REQUEST + 1).read_to_end(&mut data))
                .is_err()
            {
                continue;
            }
            // Close the file before acknowledging the message on Windows.
            if fs::remove_file(&path).is_ok()
                && data.len() as u64 <= MAX_REQUEST
                && let Ok(request) = serde_json::from_slice(&data)
            {
                requests.push(request);
            }
        }
        Ok(requests)
    }
}

fn try_claim(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_photos_out_of_mixed_requests_and_maps_the_playlist_position() {
        let files = [
            "one.MP4",
            "picture one.HEIC",
            "two.mp3",
            "picture two.PSD",
            "three.mkv",
        ]
        .map(str::to_owned);
        let (request, photos) = Request::split(files, Mode::Replace(2));
        assert_eq!(request.mode, Mode::Replace(1));
        assert_eq!(request.files.len(), 3);
        assert!(request.files[0].ends_with("one.MP4"));
        assert!(request.files[1].ends_with("two.mp3"));
        assert!(request.files[2].ends_with("three.mkv"));
        assert_eq!(photos.len(), 2);
        assert!(photos[0].ends_with("picture one.HEIC"));
        assert!(photos[1].ends_with("picture two.PSD"));
    }

    #[test]
    fn tiff_variants_and_svg_open_in_photo_windows() {
        let (request, photos) = Request::split(
            ["one.TIF", "two.tiff", "three.tff", "four.SVG"].map(str::to_owned),
            Mode::Play,
        );
        assert!(request.files.is_empty());
        assert_eq!(photos.len(), 4);
    }

    #[test]
    fn concurrent_launches_forward_once_and_the_owner_can_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Playback::open(dir.path(), &Request::default())
            .unwrap()
            .unwrap();
        let request = Request {
            files: vec!["movie with spaces.mp4".into()],
            mode: Mode::Add,
        };
        thread::scope(|scope| {
            let caller = scope.spawn(|| Playback::open(dir.path(), &request).unwrap());
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let pending = owner.pending().unwrap();
                if !pending.is_empty() {
                    assert_eq!(
                        pending,
                        [Request {
                            files: request.files.clone(),
                            mode: Mode::Add
                        }]
                    );
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "another launch did not forward its files"
                );
                thread::sleep(Duration::from_millis(5));
            }
            assert!(caller.join().unwrap().is_none());
            assert!(owner.pending().unwrap().is_empty());
        });
        drop(owner);
        assert!(
            Playback::open(dir.path(), &Request::default())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn launching_during_shutdown_does_not_lose_or_duplicate_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Playback::open(dir.path(), &Request::default())
            .unwrap()
            .unwrap();
        thread::scope(|scope| {
            let caller = scope.spawn(|| Playback::open(dir.path(), &Request::default()).unwrap());
            let until = Instant::now() + Duration::from_secs(2);
            while !fs::read_dir(dir.path().join("open-requests"))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "request"))
            {
                assert!(Instant::now() < until, "no forwarded launch to hand off");
                thread::sleep(Duration::from_millis(5));
            }
            drop(owner);
            let next = caller
                .join()
                .unwrap()
                .expect("the contender should open playback");
            assert!(next.pending().unwrap().is_empty());
        });
    }
}
