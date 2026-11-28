use image_webp::{LoopCount, WebPDecoder};
use resvg::{tiny_skia, usvg};
#[cfg(not(target_os = "macos"))]
use std::sync::{Arc, OnceLock};
use std::{
    borrow::Cow,
    fs::File,
    io::{BufReader, BufWriter, Read, Seek},
    path::Path,
};
use tempfile::NamedTempFile;

#[cfg(target_os = "macos")]
mod mac_fonts;

pub fn prepare_local(path: &Path) -> Result<Option<NamedTempFile>, String> {
    if is_local_svg(path) {
        svg_preview(path).map(Some)
    } else if let Some(preview) = animated_webp(path)? {
        Ok(Some(preview))
    } else {
        crate::heif_source::tiled_heif(path)
    }
}

const SVG_SOURCE_LIMIT: u64 = 16 * 1024 * 1024;
const SVG_MAX_SIDE: u32 = 8192;
const SVG_MAX_PIXELS: u64 = 16 * 1024 * 1024;

pub fn is_local_svg(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("svg"))
}

#[cfg(not(target_os = "macos"))]
fn svg_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTS.get_or_init(|| {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        let family = |choices: &[&str]| {
            choices.iter().find_map(|choice| {
                fonts
                    .faces()
                    .any(|face| face.families.iter().any(|(name, _)| name == choice))
                    .then(|| (*choice).to_owned())
            })
        };
        let serif = family(&[
            "Times New Roman",
            "Liberation Serif",
            "DejaVu Serif",
            "Noto Serif",
        ]);
        let sans = family(&[
            "Arial",
            "Helvetica",
            "Liberation Sans",
            "DejaVu Sans",
            "Noto Sans",
        ]);
        let mono = family(&[
            "Courier New",
            "Menlo",
            "Liberation Mono",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ]);
        if let Some(name) = serif.or_else(|| sans.clone()) {
            fonts.set_serif_family(name);
        }
        if let Some(name) = sans {
            fonts.set_sans_serif_family(name);
        }
        if let Some(name) = mono {
            fonts.set_monospace_family(name);
        }
        Arc::new(fonts)
    }))
}

fn svg_render_size(size: usvg::Size) -> (tiny_skia::IntSize, f32) {
    let (w, h) = (f64::from(size.width()), f64::from(size.height()));
    // Use the intrinsic size unless it would require a very large bitmap.
    let scale = (f64::from(SVG_MAX_SIDE) / w.max(h))
        .min((SVG_MAX_PIXELS as f64 / (w * h)).sqrt())
        .min(1.0);
    let scaled = |n: f64| (n * scale).floor().max(1.0) as u32;
    let size = tiny_skia::IntSize::from_wh(scaled(w), scaled(h)).expect("bounded SVG size");
    (size, scale as f32)
}

/// Render a static SVG without relying on an optional FFmpeg SVG decoder.
fn svg_preview(path: &Path) -> Result<NamedTempFile, String> {
    let error = |err: &dyn std::fmt::Display| format!("SVG: {err}");
    if !path.metadata().map_err(|e| error(&e))?.is_file() {
        return Err("SVG: not a regular file".into());
    }
    let mut source = Vec::new();
    File::open(path)
        .and_then(|file| file.take(SVG_SOURCE_LIMIT + 1).read_to_end(&mut source))
        .map_err(|e| error(&e))?;
    if source.len() as u64 > SVG_SOURCE_LIMIT {
        return Err("SVG: exceeds the 16 MiB input limit".into());
    }
    let embedded = usvg::ImageHrefResolver::default_data_resolver();
    let options = usvg::Options {
        font_family: "serif".into(),
        #[cfg(not(target_os = "macos"))]
        fontdb: svg_fonts(),
        #[cfg(target_os = "macos")]
        font_resolver: mac_fonts::resolver(),
        image_href_resolver: usvg::ImageHrefResolver {
            // Don't let the document read other files, or fetch them over the network.
            resolve_string: Box::new(|_, _| None),
            resolve_data: Box::new(move |mime, data, opts| {
                if !matches!(
                    mime,
                    "image/png" | "image/jpg" | "image/jpeg" | "image/gif" | "image/webp"
                ) {
                    return None;
                }
                let size = imagesize::blob_size(&data).ok()?;
                if size.width <= SVG_MAX_SIDE as usize
                    && size.height <= SVG_MAX_SIDE as usize
                    && size.width as u64 * size.height as u64 <= SVG_MAX_PIXELS
                {
                    embedded(mime, data, opts)
                } else {
                    None
                }
            }),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&source, &options).map_err(|e| error(&e))?;
    let (size, scale) = svg_render_size(tree.size());
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or("SVG: could not allocate a preview")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let output = tempfile::Builder::new()
        .prefix("coffee-")
        .suffix(".png")
        .tempfile()
        .map_err(|e| error(&e))?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(output.as_file()),
        size.width(),
        size.height(),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| error(&e))?;
    writer
        .write_image_data(&pixmap.take_demultiplied())
        .map_err(|e| error(&e))?;
    writer.finish().map_err(|e| error(&e))?;
    Ok(output)
}

/// FFmpeg before 9.0 cannot decode animated WebP. Give libmpv an equivalent
/// APNG stream without substituting the original filename in the queue.
pub fn animated_webp(path: &Path) -> Result<Option<NamedTempFile>, String> {
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("webp"))
    {
        return Ok(None);
    }
    let format_error = |err: &dyn std::fmt::Display| format!("Animated WebP: {err}");
    let mut file = BufReader::new(File::open(path).map_err(|e| format_error(&e))?);
    let mut signature = [0; 21];
    if file.read_exact(&mut signature).is_err()
        || &signature[..4] != b"RIFF"
        || &signature[8..16] != b"WEBPVP8X"
        || signature[20] & 0x02 == 0
    {
        return Ok(None);
    }
    file.rewind().map_err(|e| format_error(&e))?;
    if file
        .get_ref()
        .metadata()
        .map_err(|e| format_error(&e))?
        .len()
        > 64 * 1024 * 1024
    {
        return Err("Animated WebP exceeds the 64 MiB input limit".into());
    }
    let mut decoder = WebPDecoder::new(file).map_err(|e| format_error(&e))?;
    decoder.set_memory_limit(64 * 1024 * 1024);
    if !decoder.is_animated() {
        return Ok(None);
    }
    let count = decoder.num_frames();
    let size = decoder
        .output_buffer_size()
        .filter(|size| {
            *size <= 64 * 1024 * 1024
                && size
                    .checked_mul(count as usize)
                    .is_some_and(|bytes| bytes <= 512 * 1024 * 1024)
        })
        .ok_or("Animated WebP exceeds the decoded size limit")?;
    let plays = match decoder.loop_count() {
        LoopCount::Forever => 0,
        LoopCount::Times(n) => u32::from(n.get()),
    };
    let (w, h) = decoder.dimensions();
    let mut info = png::Info::with_size(w, h);
    info.color_type = if decoder.has_alpha() {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    };
    info.icc_profile = decoder
        .icc_profile()
        .map_err(|e| format_error(&e))?
        .map(Cow::Owned);
    let output = tempfile::Builder::new()
        .prefix("coffee-")
        .suffix(".apng")
        .tempfile()
        .map_err(|e| format_error(&e))?;
    let mut encoder = png::Encoder::with_info(BufWriter::new(output.as_file()), info)
        .map_err(|e| format_error(&e))?;
    encoder.set_compression(png::Compression::Fast);
    encoder
        .set_animated(count, plays)
        .map_err(|e| format_error(&e))?;
    let mut png = encoder.write_header().map_err(|e| format_error(&e))?;
    let mut frame = vec![0; size];
    for _ in 0..count {
        let ms = decoder
            .read_frame(&mut frame)
            .map_err(|e| format_error(&e))?;
        // As in browsers, values below 10 ms aren't treated as literal delays.
        let (delay, unit) = match ms {
            0..10 => (100, 1000),
            10..=65_535 => (ms as u16, 1000),
            _ => ((ms / 1000) as u16, 1),
        };
        png.set_frame_delay(delay, unit)
            .map_err(|e| format_error(&e))?;
        png.write_image_data(&frame).map_err(|e| format_error(&e))?;
    }
    png.finish().map_err(|e| format_error(&e))?;
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_animated_webp_to_multiple_png_frames() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/animated.webp");
        let output = animated_webp(&fixture).unwrap().unwrap();
        let png = png::Decoder::new(BufReader::new(File::open(output.path()).unwrap()))
            .read_info()
            .unwrap();
        assert_eq!((png.info().width, png.info().height), (70, 70));
        assert_eq!(png.info().animation_control.unwrap().num_frames, 2);
        assert_eq!(png.info().animation_control.unwrap().num_plays, 0);
    }

    #[test]
    fn svg_preview_uses_intrinsic_size_and_preserves_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("graphic.SVG");
        std::fs::write(
            &svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8">
            <rect width="8" height="8" fill="red" fill-opacity="0.5"/>
        </svg>"#,
        )
        .unwrap();
        let output = prepare_local(&svg).unwrap().unwrap();
        let mut png = png::Decoder::new(BufReader::new(File::open(output.path()).unwrap()))
            .read_info()
            .unwrap();
        assert_eq!((png.info().width, png.info().height), (16, 8));
        let mut pixels = vec![0; png.output_buffer_size().unwrap()];
        png.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..4], &[255, 0, 0, 128]);
        assert_eq!(&pixels[8 * 4..9 * 4], &[0; 4]);
    }

    #[test]
    fn oversized_svg_canvas_is_scaled_to_the_pixel_budget() {
        let (size, scale) = svg_render_size(usvg::Size::from_wh(100_000.0, 50_000.0).unwrap());
        assert!(scale < 1.0);
        assert!(u64::from(size.width()) * u64::from(size.height()) <= SVG_MAX_PIXELS);
        assert!(size.width().abs_diff(size.height() * 2) <= 1);
    }

    #[test]
    fn svg_does_not_read_linked_files() {
        let dir = tempfile::tempdir().unwrap();
        let linked = dir.path().join("linked.png");
        let mut encoder = png::Encoder::new(File::create(&linked).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 255]).unwrap();
        writer.finish().unwrap();
        let svg = dir.path().join("links.svg");
        let href = linked
            .to_string_lossy()
            .replace('&', "&amp;")
            .replace('"', "&quot;");
        std::fs::write(
            &svg,
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1">
                <image width="1" height="1" href="{href}"/>
                <rect x="1" width="1" height="1" fill="blue"/>
            </svg>"#
            ),
        )
        .unwrap();
        let output = prepare_local(&svg).unwrap().unwrap();
        let mut png = png::Decoder::new(File::open(output.path()).map(BufReader::new).unwrap())
            .read_info()
            .unwrap();
        let mut pixels = [0; 8];
        png.next_frame(&mut pixels).unwrap();
        assert_eq!(pixels, [0, 0, 0, 0, 0, 0, 255, 255]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn svg_renders_text_using_a_native_font() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("text.svg");
        std::fs::write(
            &svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32">
                <text x="1" y="24" font-family="sans-serif" font-weight="bold" font-size="18">Coffee</text>
            </svg>"#,
        ).unwrap();
        let output = prepare_local(&svg).unwrap().unwrap();
        let mut png = png::Decoder::new(BufReader::new(File::open(output.path()).unwrap()))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; png.output_buffer_size().unwrap()];
        png.next_frame(&mut pixels).unwrap();
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
    }
}
