//! Enumerate families using the native system font catalog on each platform.
#[derive(Default)]
pub enum Catalog {
    #[default]
    Unloaded,
    Loading,
    Ready(Vec<String>),
    Failed,
}

pub fn valid_family(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('@') && !name.chars().any(char::is_control)
}

fn families(names: Vec<String>) -> Vec<String> {
    let mut names: Vec<_> = names
        .iter()
        .map(|name| name.trim())
        .filter(|name| valid_family(name))
        .map(|name| (name.to_lowercase(), name.to_owned()))
        .collect();
    names.sort_unstable();
    names.dedup_by(|a, b| a.0 == b.0);
    names.into_iter().map(|(_, name)| name).collect()
}

pub fn discover() -> Catalog {
    match system_families() {
        Some(names) => Catalog::Ready(families(names)),
        None => Catalog::Failed,
    }
}

#[cfg(target_os = "linux")]
fn system_families() -> Option<Vec<String>> {
    let output = std::process::Command::new("fc-list")
        .args(["--format", "%{family}\n"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .flat_map(|line| line.split(','))
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn system_families() -> Option<Vec<String>> {
    use std::ffi::{CStr, c_char, c_void};
    type Object = *const c_void;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerCopyAvailableFontFamilyNames() -> Object;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: Object) -> isize;
        fn CFArrayGetValueAtIndex(array: Object, index: isize) -> Object;
        fn CFStringGetLength(string: Object) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(
            string: Object,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(object: Object);
    }

    let mut names = Vec::new();
    // The array owns its strings until CFRelease. Copy their UTF-8 bytes
    // before releasing it; the C buffer includes room for its terminator.
    unsafe {
        let list = CTFontManagerCopyAvailableFontFamilyNames();
        if list.is_null() {
            return None;
        }
        for i in 0..CFArrayGetCount(list) {
            let string = CFArrayGetValueAtIndex(list, i);
            let max = CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), UTF8);
            if let Some(size) = max.checked_add(1).filter(|&size| size > 0) {
                let mut buffer = vec![0u8; size as usize];
                if CFStringGetCString(string, buffer.as_mut_ptr().cast(), size, UTF8) != 0
                    && let Ok(name) = CStr::from_ptr(buffer.as_ptr().cast()).to_str()
                {
                    names.push(name.to_owned());
                }
            }
        }
        CFRelease(list);
    }
    Some(names)
}

#[cfg(target_os = "windows")]
fn system_families() -> Option<Vec<String>> {
    use std::{ffi::c_void, ptr};
    type Handle = *mut c_void;
    #[repr(C)]
    struct LogFont {
        metrics: [i32; 5],
        italic: u8,
        underline: u8,
        strike_out: u8,
        charset: u8,
        precision: [u8; 4],
        family: [u16; 32],
    }
    type Callback = unsafe extern "system" fn(*const LogFont, *const c_void, u32, isize) -> i32;

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn EnumFontFamiliesExW(
            dc: Handle,
            font: *mut LogFont,
            callback: Callback,
            data: isize,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetDC(window: Handle) -> Handle;
        fn ReleaseDC(window: Handle, dc: Handle) -> i32;
    }

    unsafe extern "system" fn collect(
        logfont: *const LogFont,
        _: *const c_void,
        _: u32,
        data: isize,
    ) -> i32 {
        // EnumFontFamiliesExW supplies ENUMLOGFONTEXW, which begins with
        // LOGFONTW. Enumeration is synchronous and data points to our Vec.
        let font = unsafe { &*logfont };
        let names = unsafe { &mut *(data as *mut Vec<String>) };
        let end = font.family.iter().position(|&c| c == 0).unwrap_or(32);
        if let Ok(name) = String::from_utf16(&font.family[..end]) {
            names.push(name);
        }
        1
    }

    let mut font = LogFont {
        metrics: [0; 5],
        italic: 0,
        underline: 0,
        strike_out: 0,
        charset: 1, // DEFAULT_CHARSET; empty face name lists all families.
        precision: [0; 4],
        family: [0; 32],
    };
    let mut names = Vec::<String>::new();
    unsafe {
        let dc = GetDC(ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        EnumFontFamiliesExW(
            dc,
            &mut font,
            collect,
            &mut names as *mut Vec<String> as isize,
            0,
        );
        ReleaseDC(ptr::null_mut(), dc);
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_names_are_sorted_and_deduplicated_without_losing_unicode() {
        let input = [
            " Noto Sans ",
            "Aptos",
            "noto sans",
            "عربي",
            "@Aptos",
            "",
            "bad\0name",
        ];
        assert_eq!(
            families(input.map(str::to_owned).into()),
            ["Aptos", "Noto Sans", "عربي"]
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn native_font_catalog_lists_installed_fonts() {
        assert!(matches!(discover(), Catalog::Ready(names) if !names.is_empty()));
    }
}
