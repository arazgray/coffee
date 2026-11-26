use image_webp::{LoopCount, WebPDecoder};
use std::{
    borrow::Cow,
    fs::File,
    io::{BufReader, BufWriter, Read, Seek},
    path::Path,
};
use tempfile::NamedTempFile;

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
}
