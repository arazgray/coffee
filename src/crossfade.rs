use crate::engine::{Engine, Snapshot};
use std::time::{Duration, Instant};

const BLEND: Duration = Duration::from_millis(150);
const LOAD_LIMIT: Duration = Duration::from_secs(3);

pub struct Crossfade {
    pub target: i64,
    outgoing: Snapshot,
    pixels: Vec<u8>,
    requested: Instant,
    incoming: Option<Instant>,
    last_drawn: Option<(u8, (i32, i32))>,
}

impl Crossfade {
    pub fn begin(
        mpv: &Engine,
        target: i64,
        display: (i32, i32),
        previous: Option<Self>,
    ) -> Option<Self> {
        let now = Instant::now();
        let outgoing = match previous {
            Some(mut fade) if fade.incoming.is_none() => {
                // A quick second press before the new frame arrived: keep
                // the same visible image while the newer target loads.
                fade.target = target;
                fade.requested = now;
                return Some(fade);
            }
            Some(fade) => {
                let mut shot = mpv.snapshot()?;
                // Capture what the viewer currently sees if they navigate
                // midway through another blend, not just the lower layer.
                if (shot.width, shot.height) == (fade.outgoing.width, fade.outgoing.height)
                    && let Some((alpha, _)) = fade.last_drawn
                {
                    for (dst, src) in shot.bgra.iter_mut().zip(fade.outgoing.bgra) {
                        *dst = ((u16::from(*dst) * u16::from(255 - alpha)
                            + u16::from(src) * u16::from(alpha)
                            + 127)
                            / 255) as u8;
                    }
                }
                shot
            }
            None => mpv.snapshot()?,
        };
        mpv.crossfade_bitmap(&outgoing, &outgoing.bgra, display)
            .then_some(Self {
                target,
                outgoing,
                pixels: Vec::new(),
                requested: now,
                incoming: None,
                last_drawn: Some((255, display)),
            })
    }

    pub fn ready(&mut self, id: i64, now: Instant) {
        if self.target == id && self.incoming.is_none() {
            self.incoming = Some(now);
        }
    }

    pub fn draw(&mut self, mpv: &Engine, display: (i32, i32), now: Instant) -> bool {
        let alpha = if let Some(start) = self.incoming {
            opacity(now.duration_since(start))
        } else if now.duration_since(self.requested) >= LOAD_LIMIT {
            return false;
        } else {
            255
        };
        if alpha == 0 {
            return false;
        }
        if self.last_drawn == Some((alpha, display)) {
            return true;
        }
        self.last_drawn = Some((alpha, display));
        if alpha == 255 {
            return mpv.crossfade_bitmap(&self.outgoing, &self.outgoing.bgra, display);
        }
        self.pixels.resize(self.outgoing.bgra.len(), 0);
        // The native compositor takes premultiplied BGRA. Multiplying the
        // outgoing snapshot's alpha reveals the incoming image directly.
        for (dst, &src) in self.pixels.iter_mut().zip(&self.outgoing.bgra) {
            *dst = ((u16::from(src) * u16::from(alpha) + 127) / 255) as u8;
        }
        mpv.crossfade_bitmap(&self.outgoing, &self.pixels, display)
    }
}

fn opacity(elapsed: Duration) -> u8 {
    let portion = elapsed.as_secs_f64() / BLEND.as_secs_f64();
    (255.0 * (1.0 - portion).max(0.0)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blends_directly_over_150_ms() {
        assert_eq!(opacity(Duration::ZERO), 255);
        assert_eq!(opacity(Duration::from_millis(75)), 128);
        assert_eq!(opacity(Duration::from_millis(150)), 0);
    }
}
