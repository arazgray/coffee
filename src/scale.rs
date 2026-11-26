pub const PERCENT: i32 = 110;

pub const fn pixels(logical: i32) -> i32 {
    (logical * PERCENT + 50) / 100
}
