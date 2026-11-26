//! Assemble tiled HEIC stills with libheif. On older FFmpeg builds, libmpv
//! can see the individual tiles but may select a thumbnail or a single tile.
use std::{
    borrow::Cow,
    ffi::{CStr, c_char, c_int, c_void},
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
    ptr, slice,
};
use tempfile::NamedTempFile;

type Handle = *mut c_void;
type Free = unsafe extern "C" fn(Handle);
type Accessor = unsafe extern "C" fn(Handle) -> c_int;
type ChannelAccessor = unsafe extern "C" fn(Handle, c_int) -> c_int;
type GetResource = unsafe extern "C" fn(Handle, *mut Handle) -> HeifError;

#[repr(C)]
struct HeifError {
    code: c_int,
    _subcode: c_int,
    message: *const c_char,
}

impl HeifError {
    fn check(self) -> Result<(), String> {
        if self.code == 0 {
            return Ok(());
        }
        let message = if self.message.is_null() {
            format!("error {}", self.code)
        } else {
            unsafe { CStr::from_ptr(self.message) }
                .to_string_lossy()
                .into_owned()
        };
        Err(format!("HEIF: {message}"))
    }
}

// libheif public C structs, version 1.
#[derive(Default)]
#[repr(C)]
struct Tiling {
    version: c_int,
    columns: u32,
    rows: u32,
    tile_width: u32,
    tile_height: u32,
    width: u32,
    height: u32,
    top: u32,
    left: u32,
    dimensions: u8,
    dimension_sizes: [u32; 8],
}

#[repr(C)]
struct NclxPrefix {
    _version: u8,
    primaries: c_int,
    transfer: c_int,
}

struct Resource(Handle, Free);
impl Resource {
    fn new(handle: Handle, free: Free) -> Result<Self, String> {
        if handle.is_null() {
            Err("HEIF: missing image data".into())
        } else {
            Ok(Self(handle, free))
        }
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0) }
    }
}

struct LibrarySession(unsafe extern "C" fn());
impl Drop for LibrarySession {
    fn drop(&mut self) {
        unsafe { (self.0)() }
    }
}

pub fn tiled_heif(path: &Path) -> Result<Option<NamedTempFile>, String> {
    const INPUT_LIMIT: u64 = 128 * 1024 * 1024;
    const PIXEL_LIMIT: usize = 64 * 1024 * 1024;
    const INTERLEAVED: c_int = 10;
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif"))
        || !path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= INPUT_LIMIT)
    {
        return Ok(None);
    }
    let Some(lib) = crate::platform::libheif_paths()
        .iter()
        .find_map(|path| crate::engine::load_provider(path).ok())
    else {
        return Ok(None);
    };
    macro_rules! ffi {
        ($name:literal, $signature:ty) => {
            *unsafe { lib.get::<$signature>(concat!("heif_", $name, "\0").as_bytes()) }
                .map_err(|err| format!("HEIF: {err}"))?
        };
    }
    let tiling = ffi!(
        "image_handle_get_image_tiling",
        unsafe extern "C" fn(Handle, c_int, *mut Tiling) -> HeifError
    );
    let init = ffi!("init", unsafe extern "C" fn(Handle) -> HeifError);
    let deinit = ffi!("deinit", unsafe extern "C" fn());
    let alloc = ffi!("context_alloc", unsafe extern "C" fn() -> Handle);
    let context_free = ffi!("context_free", Free);
    let read = ffi!(
        "context_read_from_memory_without_copy",
        unsafe extern "C" fn(Handle, *const u8, usize, Handle) -> HeifError
    );
    let get_primary = ffi!("context_get_primary_image_handle", GetResource);
    let handle_free = ffi!("image_handle_release", Free);
    let alpha = ffi!("image_handle_has_alpha_channel", Accessor);
    let luma = ffi!("image_handle_get_luma_bits_per_pixel", Accessor);
    let decode = ffi!(
        "decode_image",
        unsafe extern "C" fn(Handle, *mut Handle, c_int, c_int, Handle) -> HeifError
    );
    let image_free = ffi!("image_release", Free);
    let width = ffi!("image_get_width", ChannelAccessor);
    let height = ffi!("image_get_height", ChannelAccessor);
    let bits = ffi!("image_get_bits_per_pixel_range", ChannelAccessor);
    let plane = ffi!(
        "image_get_plane_readonly",
        unsafe extern "C" fn(Handle, c_int, *mut c_int) -> *const u8
    );
    let icc_size = ffi!(
        "image_handle_get_raw_color_profile_size",
        unsafe extern "C" fn(Handle) -> usize
    );
    let icc_copy = ffi!(
        "image_handle_get_raw_color_profile",
        unsafe extern "C" fn(Handle, *mut u8) -> HeifError
    );
    let nclx_get = ffi!("image_get_nclx_color_profile", GetResource);
    let nclx_free = ffi!("nclx_color_profile_free", Free);

    let io = |e: std::io::Error| format!("HEIF: {e}");
    let png_error = |e: png::EncodingError| format!("HEIF preview: {e}");
    let mut data = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(io)?;
    if data.len() as u64 > INPUT_LIMIT {
        return Ok(None);
    }
    // The context borrows data. Drop all C resources before their library.
    unsafe { init(ptr::null_mut()).check()? };
    let _session = LibrarySession(deinit);
    let ctx = Resource::new(unsafe { alloc() }, context_free)?;
    unsafe { read(ctx.0, data.as_ptr(), data.len(), ptr::null_mut()).check()? };
    let mut handle = ptr::null_mut();
    unsafe { get_primary(ctx.0, &mut handle).check()? };
    let handle = Resource::new(handle, handle_free)?;
    let mut tiled = Tiling {
        version: 1,
        ..Default::default()
    };
    unsafe { tiling(handle.0, 1, &mut tiled).check()? };
    if tiled.columns <= 1 && tiled.rows <= 1 {
        return Ok(None);
    }
    if tiled.width == 0
        || tiled.height == 0
        || (tiled.width as usize).saturating_mul(tiled.height as usize) > PIXEL_LIMIT
    {
        return Err("HEIF tile grid exceeds the decoded size limit".into());
    }
    let has_alpha = unsafe { alpha(handle.0) != 0 };
    let deep = unsafe { luma(handle.0) > 8 };
    let chroma = 10 + i32::from(has_alpha) + 2 * i32::from(deep);
    let mut img = ptr::null_mut();
    // RGB, with interleaved 8-bit or big-endian 16-bit channel storage.
    unsafe { decode(handle.0, &mut img, 1, chroma, ptr::null_mut()).check()? };
    let img = Resource::new(img, image_free)?;
    let (w, h, depth) = unsafe {
        (
            width(img.0, INTERLEAVED),
            height(img.0, INTERLEAVED),
            bits(img.0, INTERLEAVED),
        )
    };
    if w <= 0
        || h <= 0
        || (w as usize).saturating_mul(h as usize) > PIXEL_LIMIT
        || !(1..=16).contains(&depth)
    {
        return Err("HEIF returned invalid image dimensions or bit depth".into());
    }
    let row_size = w as usize * if has_alpha { 4 } else { 3 } * if deep { 2 } else { 1 };
    let mut stride = 0;
    let pixels = unsafe { plane(img.0, INTERLEAVED, &mut stride) };
    if pixels.is_null()
        || stride < row_size as c_int
        || (stride as usize)
            .checked_mul(h as usize)
            .is_none_or(|n| n > isize::MAX as usize)
    {
        return Err("HEIF returned an invalid pixel plane".into());
    }
    let mut info = png::Info::with_size(w as u32, h as u32);
    info.color_type = if has_alpha {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    };
    if deep {
        info.bit_depth = png::BitDepth::Sixteen;
    }
    let profile_size = unsafe { icc_size(handle.0) };
    if (1..=4 * 1024 * 1024).contains(&profile_size) {
        let mut profile = vec![0; profile_size];
        if unsafe { icc_copy(handle.0, profile.as_mut_ptr()).code } == 0 {
            info.icc_profile = Some(Cow::Owned(profile));
        }
    }
    if info.icc_profile.is_none() {
        let mut nclx = ptr::null_mut();
        if unsafe { nclx_get(img.0, &mut nclx).code } == 0
            && let Ok(nclx) = Resource::new(nclx, nclx_free)
        {
            let profile = unsafe { &*nclx.0.cast::<NclxPrefix>() };
            if let (Ok(p), Ok(t)) = (
                u8::try_from(profile.primaries),
                u8::try_from(profile.transfer),
            ) {
                info.coding_independent_code_points = Some(png::CodingIndependentCodePoints {
                    color_primaries: p,
                    transfer_function: t,
                    matrix_coefficients: 0,
                    is_video_full_range_image: true,
                });
            }
        }
    }
    let output = tempfile::Builder::new()
        .prefix("coffee-")
        .suffix(".png")
        .tempfile()
        .map_err(io)?;
    let mut encoder =
        png::Encoder::with_info(BufWriter::new(output.as_file()), info).map_err(png_error)?;
    encoder.set_compression(png::Compression::Fast);
    let mut png = encoder.write_header().map_err(png_error)?;
    let mut stream = png.stream_writer().map_err(png_error)?;
    let mut expanded = Vec::new();
    for y in 0..h as usize {
        let row = unsafe { slice::from_raw_parts(pixels.add(y * stride as usize), row_size) };
        if deep && depth < 16 {
            let max = (1u32 << depth) - 1;
            expanded.clear();
            for byte in row.chunks_exact(2) {
                let v = u32::from(u16::from_be_bytes([byte[0], byte[1]])).min(max);
                expanded.extend_from_slice(&((v * 65_535 / max) as u16).to_be_bytes());
            }
            stream.write_all(&expanded).map_err(io)?;
        } else {
            stream.write_all(row).map_err(io)?;
        }
    }
    stream.finish().map_err(png_error)?;
    png.finish().map_err(png_error)?;
    Ok(Some(output))
}
