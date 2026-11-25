//! Logistic squash and stretch.

const SQUASH_T: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068,
    4079, 4085, 4089, 4092, 4093, 4094,
];

/// 4096 / (1 + e^-d/256), d in -2047..=2047, by interpolation.
pub(crate) fn squash(d: i32) -> i32 {
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (SQUASH_T[i] * (128 - w) + SQUASH_T[i + 1] * w + 64) >> 7
}

pub(crate) fn stretch_table() -> Vec<i16> {
    let mut t = vec![0i16; 4096];
    let mut pi = 0usize;
    for x in -2047..=2047 {
        let v = squash(x) as usize;
        for e in t.iter_mut().take(v + 1).skip(pi) {
            *e = x as i16;
        }
        pi = pi.max(v + 1);
    }
    for e in t.iter_mut().skip(pi) {
        *e = 2047;
    }
    t
}
