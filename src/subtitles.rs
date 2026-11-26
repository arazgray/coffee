use crate::engine::Engine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ltr => "LTR",
            Self::Rtl => "RTL",
        }
    }

    // Set paragraph direction without reversing the characters or changing
    // the direction of embedded words/numbers. Escape literal ASS syntax.
    pub fn cue(self, text: &str) -> String {
        let (mark, embedding) = match self {
            Self::Ltr => ('\u{200e}', '\u{202a}'),
            Self::Rtl => ('\u{200f}', '\u{202b}'),
        };
        text.lines()
            .map(|line| {
                let safe = line.replace('\\', "\\\u{2060}").replace('{', "\\{");
                format!("{mark}{embedding}{safe}{mark}\u{202c}")
            })
            .collect::<Vec<_>>()
            .join("\\N")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub label: &'static str,
    option: &'static str,
}

pub const ENCODINGS: &[Encoding] = &[
    Encoding {
        label: "Auto detect",
        option: "auto",
    },
    Encoding {
        label: "UTF-8",
        option: "+UTF-8",
    },
    Encoding {
        label: "UTF-16 LE",
        option: "+UTF-16LE",
    },
    Encoding {
        label: "UTF-16 BE",
        option: "+UTF-16BE",
    },
    Encoding {
        label: "Windows-1252 · Western",
        option: "+CP1252",
    },
    Encoding {
        label: "Windows-1256 · Arabic",
        option: "+CP1256",
    },
    Encoding {
        label: "ISO-8859-6 · Arabic",
        option: "+ISO-8859-6",
    },
    Encoding {
        label: "Windows-1255 · Hebrew",
        option: "+CP1255",
    },
    Encoding {
        label: "Windows-1254 · Turkish",
        option: "+CP1254",
    },
    Encoding {
        label: "Windows-1251 · Cyrillic",
        option: "+CP1251",
    },
    Encoding {
        label: "Windows-1250 · Central Europe",
        option: "+CP1250",
    },
    Encoding {
        label: "ISO-8859-1 · Latin-1",
        option: "+ISO-8859-1",
    },
    Encoding {
        label: "Shift JIS · Japanese",
        option: "+SHIFT_JIS",
    },
    Encoding {
        label: "GB18030 · Chinese",
        option: "+GB18030",
    },
    Encoding {
        label: "Big5 · Traditional Chinese",
        option: "+BIG5",
    },
    Encoding {
        label: "EUC-KR · Korean",
        option: "+EUC-KR",
    },
];

fn is_text(codec: &str) -> bool {
    matches!(
        codec,
        "ass"
            | "ssa"
            | "subrip"
            | "srt"
            | "text"
            | "webvtt"
            | "mov_text"
            | "sami"
            | "ttml"
            | "microdvd"
            | "mpl2"
            | "subviewer"
            | "subviewer1"
            | "jacosub"
            | "pjs"
            | "realtext"
            | "vplayer"
            | "eia_608"
            | "hdmv_text_subtitle"
    )
}

pub struct Subtitles {
    pub direction: Option<Direction>,
    pub encoding: Encoding,
    visible: bool,
    native_visible: bool,
    current_text: bool,
    position: u8,
}

impl Default for Subtitles {
    fn default() -> Self {
        Self {
            direction: None,
            encoding: ENCODINGS[0],
            visible: true,
            native_visible: true,
            current_text: false,
            position: 100,
        }
    }
}

impl Subtitles {
    pub fn refresh(&mut self, mpv: &Engine) {
        self.current_text = mpv
            .get("current-tracks/sub/codec")
            .as_str()
            .is_some_and(is_text);
    }

    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = (self.direction != Some(direction)).then_some(direction);
    }

    pub fn set_encoding(&mut self, mpv: &Engine, encoding: Encoding) -> Result<(), String> {
        if self.encoding == encoding {
            return Ok(());
        }
        mpv.set("sub-codepage", encoding.option)?;
        self.encoding = encoding;
        let track = mpv.get("current-tracks/sub");
        if track["external"].as_bool() == Some(true)
            && track["codec"].as_str().is_some_and(is_text)
            && let Some(id) = track["id"].as_i64()
        {
            mpv.command(&["sub-reload", &id.to_string()])?;
        }
        self.refresh(mpv);
        Ok(())
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn select(&mut self, mpv: &Engine, id: i64) -> Result<(), String> {
        let id = if id == 0 { "no".into() } else { id.to_string() };
        mpv.set("sid", &id)?;
        if id != "no" {
            self.visible = true;
        }
        self.refresh(mpv);
        Ok(())
    }

    pub fn sync(&mut self, mpv: &Engine, position: u8) -> Option<String> {
        let direction = self.direction.filter(|_| self.current_text && self.visible);
        let native = self.visible && direction.is_none();
        if native != self.native_visible
            && mpv
                .set("sub-visibility", if native { "yes" } else { "no" })
                .is_ok()
        {
            self.native_visible = native;
        }
        if self.position != position && mpv.set("sub-pos", &position.to_string()).is_ok() {
            self.position = position;
        }
        let direction = direction?;
        let text = mpv.get("sub-text");
        let text = text.as_str().filter(|line| !line.trim().is_empty())?;
        Some(direction.cue(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_line_gets_a_direction_without_reversing_text_or_interpreting_ass() {
        let text = "Test (فارسی) 42!\n{\\pos(0,0)}";
        let rtl = Direction::Rtl.cue(text);
        assert!(rtl.starts_with("\u{200f}\u{202b}Test (فارسی) 42!\u{200f}\u{202c}"));
        assert!(rtl.contains("\\N\u{200f}\u{202b}\\{\\\u{2060}pos(0,0)}"));
        assert!(Direction::Ltr.cue(text).starts_with("\u{200e}\u{202a}"));
    }

    #[test]
    fn image_based_subtitles_are_left_to_mpv() {
        assert!(is_text("subrip"));
        assert!(!is_text("hdmv_pgs_subtitle"));
        assert!(!is_text("dvd_subtitle"));
    }
}
