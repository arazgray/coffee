//! Resolve only the faces used by the SVG through Core Text. Scanning all
//! system font files on first open is prohibitively slow on some Intel Macs.
use objc2::rc::Retained;
use objc2_foundation::NSString;
use resvg::usvg::{self, FontFamily, FontStyle, fontdb};
use std::{ffi::c_void, path::PathBuf, ptr, sync::Arc};

type Object = *const c_void;

#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontURLAttribute: Object;
    fn CTFontCreateWithName(name: Object, size: f64, matrix: Object) -> Object;
    fn CTFontCreateCopyWithSymbolicTraits(
        font: Object,
        size: f64,
        matrix: Object,
        value: u32,
        mask: u32,
    ) -> Object;
    fn CTFontCreateForString(font: Object, string: Object, range: Range) -> Object;
    fn CTFontCopyFamilyName(font: Object) -> Object;
    fn CTFontCopyPostScriptName(font: Object) -> Object;
    fn CTFontCopyAttribute(font: Object, attribute: Object) -> Object;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: Object);
    fn CFURLCopyFileSystemPath(url: Object, style: isize) -> Object;
}

struct Font(Object);

impl Font {
    fn named(name: &str) -> Option<Self> {
        let name = NSString::from_str(name);
        let raw =
            unsafe { CTFontCreateWithName(Retained::as_ptr(&name).cast(), 12.0, ptr::null()) };
        (!raw.is_null()).then(|| Self(raw))
    }

    fn variant(&self, spec: &usvg::Font) -> Option<Self> {
        const ITALIC: u32 = 1 << 0;
        const BOLD: u32 = 1 << 1;
        let mut traits = 0;
        if matches!(spec.style(), FontStyle::Italic | FontStyle::Oblique) {
            traits |= ITALIC;
        }
        if spec.weight() >= 600 {
            traits |= BOLD;
        }
        if traits == 0 {
            return None;
        }
        let raw =
            unsafe { CTFontCreateCopyWithSymbolicTraits(self.0, 0.0, ptr::null(), traits, traits) };
        (!raw.is_null()).then(|| Self(raw))
    }

    fn fallback(&self, ch: char) -> Option<Self> {
        let text = NSString::from_str(&ch.to_string());
        let range = Range {
            location: 0,
            length: ch.len_utf16() as isize,
        };
        let raw = unsafe { CTFontCreateForString(self.0, Retained::as_ptr(&text).cast(), range) };
        (!raw.is_null()).then(|| Self(raw))
    }

    fn matches(&self, name: &str) -> bool {
        // Core Text substitutes a default font for unknown names. Try the
        // next CSS family instead of accepting that substitution immediately.
        let same = |s: String| s.to_lowercase() == name.to_lowercase();
        unsafe { copy_string(CTFontCopyFamilyName(self.0)) }.is_some_and(same)
            || unsafe { copy_string(CTFontCopyPostScriptName(self.0)) }.is_some_and(same)
    }

    fn register(&self, db: &mut Arc<fontdb::Database>) -> Option<fontdb::ID> {
        let name = unsafe { copy_string(CTFontCopyPostScriptName(self.0)) }?;
        let find = |db: &fontdb::Database| {
            db.faces()
                .find(|face| face.post_script_name == name)
                .map(|face| face.id)
        };
        if let Some(id) = find(db) {
            return Some(id);
        }
        let path = unsafe {
            let url = CTFontCopyAttribute(self.0, kCTFontURLAttribute);
            if url.is_null() {
                return None;
            }
            let path = copy_string(CFURLCopyFileSystemPath(url, 0));
            CFRelease(url);
            PathBuf::from(path?)
        };
        Arc::make_mut(db).load_font_file(path).ok()?;
        find(db)
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

// The Copy functions above return owned CFStrings, which are toll-free
// bridged to NSString. Retained releases that ownership after copying.
unsafe fn copy_string(raw: Object) -> Option<String> {
    unsafe { Retained::<NSString>::from_raw(raw.cast_mut().cast()) }.map(|text| text.to_string())
}

pub fn resolver() -> usvg::FontResolver<'static> {
    usvg::FontResolver {
        select_font: Box::new(|spec, db| {
            let names = spec.families().iter().take(16).map(|family| match family {
                FontFamily::Serif => "Times New Roman",
                FontFamily::SansSerif => "Helvetica",
                FontFamily::Monospace => "Menlo",
                FontFamily::Cursive => "Snell Roundhand",
                FontFamily::Fantasy => "Papyrus",
                FontFamily::Named(name) => name,
            });
            for name in names.chain(["Times New Roman", "Helvetica"]) {
                let Some(font) = Font::named(name).filter(|font| font.matches(name)) else {
                    continue;
                };
                if let Some(id) = font
                    .variant(spec)
                    .and_then(|face| face.register(db))
                    .or_else(|| font.register(db))
                {
                    return Some(id);
                }
            }
            None
        }),
        select_fallback: Box::new(|ch, used, db| {
            let name = used
                .first()
                .and_then(|id| db.face(*id))?
                .post_script_name
                .clone();
            let id = Font::named(&name)?.fallback(ch)?.register(db)?;
            (!used.contains(&id)).then_some(id)
        }),
    }
}
