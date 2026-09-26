//! Faster evaluations of the float expressions in the per-pixel lookups,
//! each returning exactly what the plain expression returns.
//!
//! Rendered pixels are part of this crate's output contract (the bench pixel
//! checksums in `docs/baselines/import-checksums.txt`), so nothing here
//! approximates: every function either proves its fast result equal to the
//! plain expression's or falls back to that expression.
//!
//! - [`mul_add`]: `f32::mul_add` is a library call (software fma) on
//!   targets without the FMA instruction. The f64 sum `a*b + c` has one
//!   rounding (the product of two f32 is exact in f64); converting it to f32
//!   rounds again, which can only differ from the fused result when the f64
//!   sum lies exactly halfway between two f32 values or is not a normal f32.
//!   Those cases call `f32::mul_add`.
//! - [`round_to_isize`]: `x.round() as isize` without the library `roundf`.
//! - [`azimuth_bin`]: the 0.1-degree azimuth bin of an east/north offset.
//!   The plain path is `atan2f` (the platform's libm, which is why pixel
//!   checksums are per platform) followed by f32 degree arithmetic and a
//!   rounding to the nearest bin. The fast path computes the angle with an
//!   f64 series accurate to 1e-10 rad, uses the same f32 constants, and keeps
//!   its bin only when the bin position is more than [`BIN_MARGIN`] bins away
//!   from a rounding boundary; the plain path's own error (a few ulp of
//!   `atan2f` plus five f32 roundings) is below 1e-3 bins, so both round the
//!   same way. Positions near a boundary, zero offsets and non-finite input
//!   take the plain path.

use std::f32::consts::PI;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_6, PI as PI_F64};

/// Distance, in bins, a fast bin position must keep from a rounding
/// boundary. The plain path's worst-case error is below 1e-3 bins.
const BIN_MARGIN: f64 = 0.02;

/// `a.mul_add(b, c)`, bit for bit.
#[inline]
pub(crate) fn mul_add(a: f32, b: f32, c: f32) -> f32 {
    let sum = f64::from(a) * f64::from(b) + f64::from(c);
    let bits = sum.to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    // A normal f32 magnitude (biased f64 exponent 1023-126 ..= 1023+127),
    // and not an f32 halfway point (the 29 low mantissa bits are 1 then 0s).
    if (897..=1150).contains(&exponent) && bits & 0x1fff_ffff != 0x1000_0000 {
        sum as f32
    } else {
        a.mul_add(b, c)
    }
}

/// `x.round() as isize`, bit for bit: half away from zero, NaN to 0,
/// saturating at the `isize` bounds.
#[inline]
pub(crate) fn round_to_isize(x: f32) -> isize {
    let truncated = x as isize;
    // Exact: below 2^23 `truncated` and `x` share sign and binade (or
    // `truncated` is 0), and above it `x` is an integer.
    let fraction = x - truncated as f32;
    if fraction >= 0.5 {
        truncated.saturating_add(1)
    } else if fraction <= -0.5 {
        truncated.saturating_sub(1)
    } else {
        truncated
    }
}

/// The plain azimuth of an east/north offset, degrees in `[0, 360]`.
#[inline]
pub(crate) fn azimuth_from_xy(east: f32, north: f32) -> f32 {
    let mut degrees = east.atan2(north) * 180.0 / PI;
    if degrees < 0.0 {
        degrees += 360.0;
    }
    degrees
}

/// The plain bin of an azimuth in degrees: `round(azimuth mod 360 / width)`
/// modulo `bins`.
#[inline]
pub(crate) fn plain_azimuth_bin(azimuth_deg: f32, bin_width_deg: f32, bins: usize) -> usize {
    ((azimuth_deg.rem_euclid(360.0) / bin_width_deg).round() as usize) % bins
}

/// `plain_azimuth_bin(azimuth_from_xy(east, north), ...)`, exactly.
#[inline]
pub(crate) fn azimuth_bin(east: f32, north: f32, bin_width_deg: f32, bins: usize) -> usize {
    match fast_azimuth_bin(east, north, bin_width_deg, bins) {
        Some(bin) => bin,
        None => plain_azimuth_bin(azimuth_from_xy(east, north), bin_width_deg, bins),
    }
}

/// The bin through the f64 series, or `None` when the bin position lies
/// within [`BIN_MARGIN`] of a rounding boundary or the input is a zero or
/// non-finite offset.
#[inline]
fn fast_azimuth_bin(east: f32, north: f32, bin_width_deg: f32, bins: usize) -> Option<usize> {
    // Finite and nonzero: magnitude bits in 1 ..= f32::MAX's.
    let finite_nonzero = |value: f32| (value.to_bits() & 0x7fff_ffff).wrapping_sub(1) < 0x7f7f_ffff;
    if !(finite_nonzero(east) && finite_nonzero(north)) {
        return None;
    }
    // The plain path's constants (f32 180 / PI and f32 bin width) folded
    // into one scale; the folding changes the position by about 1e-13 bins.
    let bin_width = f64::from(bin_width_deg);
    let scale = 180.0 / f64::from(PI) / bin_width;
    let mut position = atan2(f64::from(east), f64::from(north)) * scale;
    if position < 0.0 {
        position += 360.0 / bin_width;
    }
    // `position` is in [0, 360 / width]: truncation is a rounding down.
    if !(0.0..4.0e9).contains(&position) {
        return None;
    }
    let nearest = (position + 0.5) as i64;
    if (position - nearest as f64).abs() > 0.5 - BIN_MARGIN {
        return None;
    }
    Some(nearest as usize % bins)
}

/// `atan2(y, x)` in f64 for finite, nonzero `y` and `x`, within 1e-10 rad.
#[inline]
fn atan2(y: f64, x: f64) -> f64 {
    let (ay, ax) = (y.abs(), x.abs());
    let mut angle = if ay <= ax {
        atan_unit(ay / ax)
    } else {
        FRAC_PI_2 - atan_unit(ax / ay)
    };
    if x < 0.0 {
        angle = PI_F64 - angle;
    }
    if y < 0.0 { -angle } else { angle }
}

/// `atan(t)` for `t` in `[0, 1]`: the series directly up to tan(pi/12),
/// above it after the reduction atan(t) = pi/6 + atan((t - s)/(1 + t s))
/// with s = tan(pi/6), which keeps the series argument within tan(pi/12).
#[inline]
fn atan_unit(t: f64) -> f64 {
    const TAN_PI_12: f64 = 0.267_949_192_431_122_7;
    const TAN_PI_6: f64 = 0.577_350_269_189_625_8;
    if t <= TAN_PI_12 {
        atan_series(t)
    } else {
        FRAC_PI_6 + atan_series((t - TAN_PI_6) / (1.0 + t * TAN_PI_6))
    }
}

/// The alternating series of `atan(u)` through `u^15` for `|u| <= tan(pi/12)`;
/// the first omitted term bounds the error by `0.268^17 / 17 < 2e-11`.
#[inline]
fn atan_series(u: f64) -> f64 {
    let u2 = u * u;
    u * (1.0
        - u2 * (1.0 / 3.0
            - u2 * (1.0 / 5.0
                - u2 * (1.0 / 7.0
                    - u2 * (1.0 / 9.0
                        - u2 * (1.0 / 11.0 - u2 * (1.0 / 13.0 - u2 * (1.0 / 15.0))))))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn f32_in(&mut self, low: f32, high: f32) -> f32 {
            let unit = (self.next() >> 40) as f32 / (1u64 << 24) as f32;
            low + (high - low) * unit
        }
    }

    #[test]
    fn mul_add_is_the_fused_result() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let specials = [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            f32::MIN_POSITIVE,
            1.0e-40,
            f32::MAX,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            0.5,
            1.0 + f32::EPSILON,
        ];
        for &a in &specials {
            for &b in &specials {
                for &c in &specials {
                    assert_eq!(
                        mul_add(a, b, c).to_bits(),
                        a.mul_add(b, c).to_bits(),
                        "{a} {b} {c}"
                    );
                }
            }
        }
        for _ in 0..2_000_000 {
            let a = f32::from_bits(rng.next() as u32);
            let b = f32::from_bits(rng.next() as u32);
            let c = f32::from_bits(rng.next() as u32);
            let (fast, plain) = (mul_add(a, b, c), a.mul_add(b, c));
            assert!(
                fast.to_bits() == plain.to_bits() || (fast.is_nan() && plain.is_nan()),
                "{a:e} {b:e} {c:e}"
            );
            // Pixel-scale values: squared kilometre offsets.
            let dx = rng.f32_in(-600.0, 600.0);
            let dy = rng.f32_in(-600.0, 600.0);
            assert_eq!(
                mul_add(dx, dx, dy * dy).to_bits(),
                dx.mul_add(dx, dy * dy).to_bits()
            );
        }
    }

    #[test]
    fn round_to_isize_is_round_then_cast() {
        let mut rng = Rng(0x0123_4567_89ab_cdef);
        let specials = [
            0.0f32,
            -0.0,
            0.5,
            -0.5,
            1.5,
            2.5,
            -2.5,
            0.499_999_97,
            -0.499_999_97,
            8_388_607.5,
            8_388_608.0,
            16_777_217.0,
            1.0e20,
            -1.0e20,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::MIN_POSITIVE,
        ];
        for &x in &specials {
            assert_eq!(round_to_isize(x), x.round() as isize, "{x}");
        }
        for _ in 0..2_000_000 {
            let x = f32::from_bits(rng.next() as u32);
            assert_eq!(round_to_isize(x), x.round() as isize, "{x:e}");
            let x = rng.f32_in(-2000.0, 2000.0);
            assert_eq!(round_to_isize(x), x.round() as isize, "{x}");
        }
        // Every value in [-4, 4] at f32 resolution near the halves.
        let mut x = -4.0f32;
        while x <= 4.0 {
            assert_eq!(round_to_isize(x), x.round() as isize, "{x}");
            x = f32::from_bits(if x >= 0.0 {
                x.to_bits() + 97
            } else {
                x.to_bits() - 97
            });
            if x.is_nan() || x.abs() < 1e-30 {
                x = 1e-30;
            }
        }
    }

    /// The fast bin equals the plain bin: random offsets at every scale the
    /// renderer uses, every pixel of a large rotated viewport, the axes and
    /// diagonals, and offsets that sit exactly on bin boundaries. Also
    /// measures how far the fast position is from the plain one, which must
    /// stay well inside the margin.
    #[test]
    fn azimuth_bin_is_the_plain_bin() {
        const WIDTH: f32 = 0.1;
        const BINS: usize = 3600;
        let check = |east: f32, north: f32| {
            assert_eq!(
                azimuth_bin(east, north, WIDTH, BINS),
                plain_azimuth_bin(azimuth_from_xy(east, north), WIDTH, BINS),
                "east {east:e} north {north:e}"
            );
        };
        let mut rng = Rng(0xdead_beef_cafe_f00d);
        let mut fast = 0usize;
        let mut worst = 0.0f64;
        for _ in 0..3_000_000 {
            for scale in [1.0e-3f32, 1.0, 300.0, 1.0e5] {
                let east = rng.f32_in(-scale, scale);
                let north = rng.f32_in(-scale, scale);
                check(east, north);
                if fast_azimuth_bin(east, north, WIDTH, BINS).is_some() {
                    fast += 1;
                    let plain = f64::from(azimuth_from_xy(east, north).rem_euclid(360.0))
                        / f64::from(WIDTH);
                    let mut degrees =
                        atan2(f64::from(east), f64::from(north)) * 180.0 / f64::from(PI);
                    if degrees < 0.0 {
                        degrees += 360.0;
                    }
                    let mut diff = (degrees / f64::from(WIDTH) - plain).abs();
                    if diff > 1800.0 {
                        diff = (diff - 3600.0).abs();
                    }
                    worst = worst.max(diff);
                }
            }
        }
        assert!(fast > 10_000_000, "fast path taken {fast} times");
        assert!(
            worst < BIN_MARGIN / 5.0,
            "fast and plain bin positions differ by {worst} bins"
        );

        // A 2560x1440 viewport at 0.25 km/px, rotated as the bench does,
        // and at the rotations the app quantizes to.
        for rotation in [0.0f32, 0.02, -0.035, 0.5] {
            let (sin, cos) = rotation.sin_cos();
            for y in (0..1440u32).step_by(3) {
                let dy = (792.0 - (y as f32 + 0.5)) * 0.25;
                for x in 0..2560u32 {
                    let dx = (x as f32 + 0.5 - 1280.0) * 0.25;
                    check(dx * cos - dy * sin, dx * sin + dy * cos);
                }
            }
        }

        // Axes, diagonals, signed zeros and non-finite offsets.
        for (east, north) in [
            (0.0f32, 1.0f32),
            (1.0, 0.0),
            (0.0, -1.0),
            (-1.0, 0.0),
            (-0.0, -1.0),
            (-0.0, -0.0),
            (0.0, 0.0),
            (1.0, 1.0),
            (-1.0, -1.0),
            (1.0e-30, -1.0),
            (-1.0e-30, -1.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
            (f32::INFINITY, f32::INFINITY),
        ] {
            check(east, north);
        }

        // Offsets whose plain position is closest to a boundary: bin edges
        // at 0.05 degree steps around the circle.
        for step in 0..7200 {
            let degrees = step as f64 * 0.05 + 0.025;
            let radians = degrees.to_radians();
            for radius in [1.0f64, 123.4, 460.0] {
                check(
                    (radius * radians.sin()) as f32,
                    (radius * radians.cos()) as f32,
                );
            }
        }
    }
}
