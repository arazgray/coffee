//! Bounded background read-ahead. libmpv keeps its native decoder and video
//! surface, but reads cached files through a seekable client stream.
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, c_char, c_int, c_void},
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::SystemTime,
};

const BUDGET: usize = 256 * 1024 * 1024;
const FILE_LIMIT: usize = 128 * 1024 * 1024;

pub struct Asset {
    bytes: Vec<u8>,
    stamp: (u64, Option<SystemTime>),
    extension: String,
    pub animated_webp: bool,
}

impl Asset {
    fn read(path: &Path, limit: usize) -> Option<Self> {
        let stamp = stamp(&path.metadata().ok()?)?;
        if usize::try_from(stamp.0).ok()? > limit {
            return None;
        }
        let preview = crate::image_source::prepare_local(path).ok()?;
        let source = preview.as_ref().map_or(path, |preview| preview.path());
        let extension = source.extension()?.to_str()?.to_ascii_lowercase();
        if !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        let file = File::open(source).ok()?;
        let size = usize::try_from(file.metadata().ok()?.len()).ok()?;
        if size > limit {
            return None;
        }
        let mut bytes = Vec::with_capacity(size);
        file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.is_empty() || bytes.len() > limit {
            return None;
        }
        Some(Self {
            bytes,
            stamp,
            animated_webp: preview.is_some() && extension == "apng",
            extension,
        })
    }
}

fn stamp(meta: &Metadata) -> Option<(u64, Option<SystemTime>)> {
    meta.is_file().then(|| (meta.len(), meta.modified().ok()))
}

struct State {
    wanted: Vec<PathBuf>,
    cached: HashMap<PathBuf, Arc<Asset>>,
    attempted: HashSet<PathBuf>,
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    budget: usize,
}

pub struct Cache {
    shared: Arc<Shared>,
}

impl Cache {
    pub fn new() -> Self {
        Self::with_budget(BUDGET)
    }

    fn with_budget(budget: usize) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                wanted: Vec::new(),
                cached: HashMap::new(),
                attempted: HashSet::new(),
                stopped: false,
            }),
            changed: Condvar::new(),
            budget,
        });
        let worker = Arc::clone(&shared);
        if let Err(error) = thread::Builder::new()
            .name("coffee-preload".into())
            .spawn(move || warm(worker))
        {
            eprintln!("Can't start photo read-ahead: {error}");
        }
        Self { shared }
    }

    pub fn plan(&self, wanted: Vec<PathBuf>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.wanted == wanted {
            return;
        }
        state.cached.retain(|path, _| wanted.contains(path));
        state.attempted.clear();
        state.wanted = wanted;
        drop(state);
        self.shared.changed.notify_one();
    }

    pub fn get(&self, path: &Path) -> Option<Arc<Asset>> {
        let entry = self.shared.state.lock().ok()?.cached.get(path).cloned()?;
        (stamp(&fs::metadata(path).ok()?)? == entry.stamp).then_some(entry)
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stopped = true;
        self.shared.changed.notify_one();
    }
}

fn warm(shared: Arc<Shared>) {
    loop {
        let (path, limit) = {
            let mut state = shared.state.lock().unwrap();
            let path = loop {
                if state.stopped {
                    return;
                }
                if let Some(path) = state
                    .wanted
                    .iter()
                    .find(|&p| !state.cached.contains_key(p) && !state.attempted.contains(p))
                {
                    break path.clone();
                }
                state = shared.changed.wait(state).unwrap();
            };
            // Reserve space for earlier (closer or currently relevant) media.
            let used: usize = state
                .wanted
                .iter()
                .take_while(|p| *p != &path)
                .filter_map(|p| state.cached.get(p))
                .map(|a| a.bytes.len())
                .sum();
            (path, shared.budget.saturating_sub(used).min(FILE_LIMIT))
        };
        let entry = if limit > 0 {
            Asset::read(&path, limit)
        } else {
            None
        };
        let mut state = shared.state.lock().unwrap();
        if state.stopped || !state.wanted.contains(&path) {
            continue;
        }
        state.attempted.insert(path.clone());
        if let Some(entry) = entry {
            let mut remaining = shared.budget;
            for candidate in state.wanted.clone() {
                if candidate == path {
                    if let Some(left) = remaining.checked_sub(entry.bytes.len()) {
                        remaining = left;
                    } else {
                        break;
                    }
                } else if let Some(prior) = state.cached.get(&candidate) {
                    if let Some(left) = remaining.checked_sub(prior.bytes.len()) {
                        remaining = left;
                    } else {
                        state.cached.remove(&candidate);
                    }
                }
            }
            let used: usize = state.cached.values().map(|a| a.bytes.len()).sum();
            if used + entry.bytes.len() <= shared.budget {
                state.cached.insert(path, Arc::new(entry));
            }
        }
    }
}

type Cookie = *mut c_void;
type ReadFn = unsafe extern "C" fn(Cookie, *mut c_char, u64) -> i64;
type SeekFn = unsafe extern "C" fn(Cookie, i64) -> i64;
type SizeFn = unsafe extern "C" fn(Cookie) -> i64;
type CloseFn = unsafe extern "C" fn(Cookie);
pub type OpenFn = unsafe extern "C" fn(Cookie, *mut c_char, *mut StreamInfo) -> c_int;

#[repr(C)]
pub struct StreamInfo {
    cookie: Cookie,
    read: ReadFn,
    seek: SeekFn,
    size: SizeFn,
    close: CloseFn,
    cancel: Option<CloseFn>,
}

#[derive(Default)]
pub struct StreamSlot {
    stream: Mutex<(u64, Option<(String, Arc<Asset>)>)>,
}

impl StreamSlot {
    pub fn stage(&self, asset: Arc<Asset>) -> String {
        let mut slot = self.stream.lock().unwrap();
        slot.0 = slot.0.wrapping_add(1);
        // Keep a suffix for demuxers that use the filename to decide how
        // to probe. `path` and the visible playlist still name the original.
        let uri = format!("coffee-ram://{}/media.{}", slot.0, asset.extension);
        slot.1 = Some((uri.clone(), asset));
        uri
    }

    pub fn clear(&self) {
        self.stream.lock().unwrap().1 = None;
    }
}

struct Cursor {
    asset: Arc<Asset>,
    pos: usize,
}

/// libmpv keeps `user` until `mpv_terminate_destroy` has finished. Only one
/// read/seek runs on a given cookie at a time; close returns ownership.
pub unsafe extern "C" fn open(user: Cookie, uri: *mut c_char, info: *mut StreamInfo) -> c_int {
    let slot = unsafe { &*user.cast::<StreamSlot>() };
    let uri = unsafe { CStr::from_ptr(uri) }.to_bytes();
    let Some(asset) = slot.stream.lock().ok().and_then(|slot| {
        let (expected, asset) = slot.1.as_ref()?;
        (expected.as_bytes() == uri).then(|| Arc::clone(asset))
    }) else {
        return -13; // MPV_ERROR_LOADING_FAILED
    };
    let cursor = Box::new(Cursor { asset, pos: 0 });
    unsafe {
        info.write(StreamInfo {
            cookie: Box::into_raw(cursor).cast(),
            read,
            seek,
            size,
            close,
            cancel: None,
        })
    };
    0
}

unsafe extern "C" fn read(cookie: Cookie, buf: *mut c_char, count: u64) -> i64 {
    let cursor = unsafe { &mut *cookie.cast::<Cursor>() };
    let count =
        (cursor.asset.bytes.len() - cursor.pos).min(usize::try_from(count).unwrap_or(usize::MAX));
    if count > 0 {
        unsafe {
            ptr::copy_nonoverlapping(
                cursor.asset.bytes.as_ptr().add(cursor.pos),
                buf.cast(),
                count,
            )
        };
        cursor.pos += count;
    }
    count as i64
}

unsafe extern "C" fn seek(cookie: Cookie, offset: i64) -> i64 {
    let cursor = unsafe { &mut *cookie.cast::<Cursor>() };
    match usize::try_from(offset) {
        Ok(pos) if pos <= cursor.asset.bytes.len() => {
            cursor.pos = pos;
            offset
        }
        _ => -20, // MPV_ERROR_GENERIC
    }
}

unsafe extern "C" fn size(cookie: Cookie) -> i64 {
    unsafe { &*cookie.cast::<Cursor>() }.asset.bytes.len() as i64
}

unsafe extern "C" fn close(cookie: Cookie) {
    drop(unsafe { Box::from_raw(cookie.cast::<Cursor>()) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_seekable_after_cache_slot_is_replaced() {
        let asset = Arc::new(Asset {
            bytes: vec![10, 20, 30, 40],
            stamp: (4, None),
            extension: "mov".to_owned(),
            animated_webp: false,
        });
        let slot = StreamSlot::default();
        let uri = std::ffi::CString::new(slot.stage(asset)).unwrap();
        let mut info = std::mem::MaybeUninit::uninit();
        unsafe {
            assert_eq!(
                open(
                    ptr::from_ref(&slot).cast_mut().cast(),
                    uri.as_ptr().cast_mut(),
                    info.as_mut_ptr()
                ),
                0
            );
            let stream = info.assume_init();
            slot.clear();
            assert_eq!((stream.size)(stream.cookie), 4);
            assert_eq!((stream.seek)(stream.cookie, 2), 2);
            let mut buf = [0u8; 3];
            assert_eq!((stream.read)(stream.cookie, buf.as_mut_ptr().cast(), 3), 2);
            assert_eq!(buf, [30, 40, 0]);
            assert_eq!((stream.read)(stream.cookie, buf.as_mut_ptr().cast(), 3), 0);
            assert_eq!((stream.seek)(stream.cookie, -1), -20);
            (stream.close)(stream.cookie);
        }
    }
}
