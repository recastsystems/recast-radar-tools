//! The per-pixel `atan2`, in plain f64 arithmetic.
//!
//! [`atan2`] takes f32 arguments and returns the f64 angle; the renderer
//! rounds it to f32. It uses only IEEE 754 addition, subtraction,
//! multiplication and division, which Rust evaluates without contraction or
//! extended precision, so the result is the same bits on every target. A
//! C-library `atan2` or `atan2f` is not: their last bits differ between the
//! Windows CRT, glibc, musl and macOS, and a one-ulp difference moves a pixel
//! that sits on an azimuth bin boundary into the next bin.
//!
//! Method. With `n = min(|y|, |x|)` and `d = max(|y|, |x|)`, the angle
//! `atan(n / d)` lies in `[0, pi/4]`. It is `atan(c) + atan(u)` with
//! `u = (n - c d) / (d + c n)`, where `c` is 0 up to `n / d = tan(pi/16)`,
//! 13/32 up to `tan(3pi/16)` and 1 above, so `|u| <= 0.2061`. For f32
//! inputs every product and sum in `u` is exact in f64 (`c` has at most five
//! significant bits and `n / d` is bounded in each branch), so `u` is one
//! correctly rounded division. The octant and quadrant fold in as `base ± p`
//! with a correctly rounded constant `base` per branch and quadrant (0,
//! `atan(13/32)`, pi/4 and their reflections about pi/2 and pi).
//!
//! `atan(u) = u + u^3 P(u^2)`, where `P` is a degree-7 polynomial fitted to
//! `(atan(sqrt(s)) - sqrt(s)) / s^1.5` on `s` in `[0, 0.04249]` with
//! mpmath's `chebyfit` (300-bit arithmetic), evaluated in Estrin's scheme;
//! the approximation error on `P` is 1.5e-17. The bases are mpmath values
//! rounded to nearest. Measured against mpmath's atan2 on 400,000 f32
//! argument pairs (pixel-scale, every binade, and ratios within 1e-5 of the
//! three branch points), the largest error is 1.82 ulp of the f64 result;
//! against the MSVC CRT's f64 `atan2` on 106 million pairs, at most 2 ulp,
//! and the f32 roundings of the two agree on every pair.
//!
//! Zeros, infinities and NaN return what C99 Annex F specifies for `atan2`.
//!
//! Cost: one division and about 30 other f64 operations, inlined. On the
//! bench's 6.7 million viewport offsets, pinned to one core of a Ryzen 9
//! 9950X3D, it takes 3.5 ns a call, against 3.7 ns for the MSVC CRT's
//! `atan2f` and 5.7 ns for its f64 `atan2` (`docs/perf/render-atan2.md`).

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

/// 3pi/4, correctly rounded (`3.0 * FRAC_PI_4` is the same bits).
const FRAC_3PI_4: f64 = f64::from_bits(0x4002_d97c_7f33_21d2);

/// tan(pi/16) and tan(3pi/16): the ratios `n / d` at which `c` changes. Their
/// rounding only moves the switch points; every branch is accurate a little
/// past its end.
const TAN_PI_16: f64 = 0.198_912_367_379_658;
const TAN_3PI_16: f64 = 0.668_178_637_919_298_9;

/// The middle branch's `c`: 13/32, near tan(pi/8) and exact in five bits.
const C_MID: f64 = 0.406_25;

/// `P(s)`, constant term first.
const P: [f64; 8] = [
    f64::from_bits(0xbfd5_5555_5555_5555), // -0.3333333333333333
    f64::from_bits(0x3fc9_9999_9999_9363), //  0.19999999999995585
    f64::from_bits(0xbfc2_4924_923d_2160), // -0.14285714283529227
    f64::from_bits(0x3fbc_71c7_0ab8_0ac1), //  0.11111110698406447
    f64::from_bits(0xbfb7_45cb_04fe_5f76), // -0.09090870735087528
    f64::from_bits(0x3fb3_aff5_4086_54a6), //  0.07690365624724169
    f64::from_bits(0xbfb0_ed24_24be_ea86), // -0.06611848733056477
    f64::from_bits(0x3fa9_edae_2284_f12c), //  0.05064148112657771
];

/// `base` by `3 * quadrant + branch`, where `quadrant` is
/// `swapped + 2 * (x < 0)` and `branch` is 0, 1 or 2 for `c` = 0, 13/32, 1.
/// Quadrants 1 and 2 subtract `atan(u)`, 0 and 3 add it.
const BASE: [f64; 12] = [
    // |y| <= |x|, x > 0: atan(c)
    0.0,
    f64::from_bits(0x3fd8_b24d_394a_1b25), // 0.38588266939807375
    FRAC_PI_4,
    // |y| > |x|, x > 0: pi/2 - atan(c)
    FRAC_PI_2,
    f64::from_bits(0x3ff2_f568_05f1_a64f), // 1.1849136573968229
    FRAC_PI_4,
    // |y| <= |x|, x < 0: pi - atan(c)
    PI,
    f64::from_bits(0x4006_0bb1_ad1a_e9b4), // 2.7557099841917196
    FRAC_3PI_4,
    // |y| > |x|, x < 0: pi/2 + atan(c)
    FRAC_PI_2,
    f64::from_bits(0x3fff_4e8e_a296_b3e2), // 1.9566789961929705
    FRAC_3PI_4,
];

/// `atan2(y, x)` in radians, in `[-pi, pi]`, within 2 ulp of the exact
/// value and the same bits on every target.
#[inline]
pub(crate) fn atan2(y: f32, x: f32) -> f64 {
    // Finite and nonzero: magnitude bits in 1 ..= f32::MAX's.
    let finite_nonzero = |value: f32| (value.to_bits() & 0x7fff_ffff).wrapping_sub(1) < 0x7f7f_ffff;
    if !(finite_nonzero(y) && finite_nonzero(x)) {
        return special(y, x);
    }
    let (ay, ax) = (f64::from(y.abs()), f64::from(x.abs()));
    let swapped = ay > ax;
    let n = if swapped { ax } else { ay };
    let d = if swapped { ay } else { ax };
    let mid = n > TAN_PI_16 * d;
    let high = n > TAN_3PI_16 * d;
    let c = if high {
        1.0
    } else if mid {
        C_MID
    } else {
        0.0
    };
    // One division: the branches differ only in `c`.
    let u = (n - c * d) / (d + c * n);
    let s = u * u;
    let s2 = s * s;
    let s4 = s2 * s2;
    let poly = (P[0] + s * P[1])
        + s2 * (P[2] + s * P[3])
        + s4 * ((P[4] + s * P[5]) + s2 * (P[6] + s * P[7]));
    let atan_u = u + u * s * poly;
    let quadrant = usize::from(swapped) + 2 * usize::from(x < 0.0);
    let signed = if quadrant == 1 || quadrant == 2 {
        -atan_u
    } else {
        atan_u
    };
    let angle = BASE[3 * quadrant + usize::from(mid) + usize::from(high)] + signed;
    if y < 0.0 { -angle } else { angle }
}

/// C99 Annex F `atan2` for a zero, infinite or NaN argument.
#[cold]
fn special(y: f32, x: f32) -> f64 {
    if y.is_nan() || x.is_nan() {
        return f64::NAN;
    }
    let angle = if y == 0.0 {
        if x.is_sign_negative() { PI } else { 0.0 }
    } else if x == 0.0 {
        FRAC_PI_2
    } else if y.is_infinite() {
        if x == f32::INFINITY {
            FRAC_PI_4
        } else if x == f32::NEG_INFINITY {
            FRAC_3PI_4
        } else {
            FRAC_PI_2
        }
    } else if x.is_sign_negative() {
        PI
    } else {
        0.0
    };
    angle.copysign(f64::from(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distance in ulps between two finite f64 of the same sign.
    fn ulps(a: f64, b: f64) -> u64 {
        assert_eq!(a.is_sign_negative(), b.is_sign_negative(), "{a} {b}");
        a.abs().to_bits().abs_diff(b.abs().to_bits())
    }

    /// Correctly rounded values from mpmath (atan2 at 300 bits, rounded to
    /// nearest f64): the branch points and their neighbours, octant and quadrant boundaries,
    /// pixel-scale offsets and extreme binades. Each is within 2 ulp.
    #[test]
    fn matches_mpmath_reference_values() {
        let cases: [(u32, u32, u64); 36] = [
            (0x3f80_0000, 0x3f80_0000, 0x3fe9_21fb_5444_2d18), // (1, 1)
            (0x3f80_0000, 0x4000_0000, 0x3fdd_ac67_0561_bb4f), // (1, 2)
            (0x4000_0000, 0x3f80_0000, 0x3ff1_b6e1_92eb_be44), // (2, 1)
            (0xbf80_0000, 0x4000_0000, 0xbfdd_ac67_0561_bb4f), // (-1, 2)
            (0x3f80_0000, 0xc000_0000, 0x4005_6c6e_7397_f5ae), // (1, -2)
            (0xc040_0000, 0xc0e0_0000, 0xc005_e4c3_6ca0_118a), // (-3, -7)
            (0x3ed4_13cc, 0x3f80_0000, 0x3fd9_21fb_3e15_8a9b), // just below tan(pi/8)
            (0x3ed4_13cd, 0x3f80_0000, 0x3fd9_21fb_5965_d9d1), // just above tan(pi/8)
            (0x3f80_0000, 0x3ed4_13cd, 0x3ff2_d97c_7dea_b6a4), // (1, tan(pi/8))
            (0xbf80_0000, 0xbed4_13cd, 0xbfff_6a7a_2a9d_a38d), // (-1, -tan(pi/8))
            (0x0da2_4260, 0x3f80_0000, 0x39b4_484c_0000_0000), // (1e-30, 1)
            (0x3f80_0000, 0x0da2_4260, 0x3ff9_21fb_5444_2d18), // (1, 1e-30)
            (0x8da2_4260, 0xbf80_0000, 0xc009_21fb_5444_2d18), // (-1e-30, -1)
            (0x7f61_b1e6, 0x006c_e3ee, 0x3ff9_21fb_5444_2d18), // (3e38, 1e-38)
            (0x0000_0001, 0x3f80_0000, 0x36a0_0000_0000_0000), // (smallest subnormal, 1)
            (0x42f6_e979, 0xc423_948b, 0x4007_a40f_1821_823a), // (123.456, -654.321)
            (0xbe00_0000, 0x3d80_0000, 0xbff1_b6e1_92eb_be44), // (-0.125, 0.0625)
            (0x43e5_f000, 0x3e00_0000, 0x3ff9_20de_5006_02ad), // (459.875, 0.125)
            (0xbe80_0000, 0xc3e5_f000, 0xc009_20de_5007_63f7), // (-0.25, -459.875)
            (0x40e0_0000, 0x41c0_0000, 0x3fd2_29ae_c476_38dd), // (7, 24)
            (0x41c0_0000, 0xc0e0_0000, 0x3ffd_ac67_0561_bb4f), // (24, -7)
            (0xc348_8000, 0x4347_8000, 0xbfe9_3676_32c0_07ed), // (-200.5, 199.5)
            (0x4347_8000, 0x4348_8000, 0x3fe9_0d80_75c8_5244), // (199.5, 200.5)
            (0x3f80_0000, 0x4040_0000, 0x3fd4_978f_a326_9ee1), // (1, 3)
            (0xc000_0000, 0x4040_0000, 0xbfe2_d0ea_d606_6395), // (-2, 3)
            (0x4000_0000, 0x4040_0000, 0x3fe2_d0ea_d606_6395), // (2, 3)
            (0x3e4b_afae, 0x3f80_0000, 0x3fc9_21fb_352a_03e9), // around tan(pi/16)
            (0x3e4b_afaf, 0x3f80_0000, 0x3fc9_21fb_53f2_39d2),
            (0x3e4b_afb0, 0x3f80_0000, 0x3fc9_21fb_72ba_6fb8),
            (0x3ecf_ffff, 0x3f80_0000, 0x3fd8_b24d_1dd2_9503), // around 13/32
            (0x3ed0_0000, 0x3f80_0000, 0x3fd8_b24d_394a_1b25),
            (0x3ed0_0001, 0x3f80_0000, 0x3fd8_b24d_54c1_a13e),
            (0x3f2b_0dc0, 0x3f80_0000, 0x3fe2_d97c_61aa_a2df), // around tan(3pi/16)
            (0x3f2b_0dc1, 0x3f80_0000, 0x3fe2_d97c_77ca_1b99),
            (0x3f2b_0dc2, 0x3f80_0000, 0x3fe2_d97c_8de9_9440),
            (0xbf2b_0dc1, 0xbf80_0000, 0xc004_6b9c_3651_a632), // (-tan(3pi/16), -1)
        ];
        for (y, x, expected) in cases {
            let (y, x, expected) = (
                f32::from_bits(y),
                f32::from_bits(x),
                f64::from_bits(expected),
            );
            let got = atan2(y, x);
            assert!(
                ulps(got, expected) <= 2,
                "atan2({y:e}, {x:e}) = {got:e}, expected {expected:e}"
            );
        }
    }

    /// Zeros, infinities and NaN, as C99 Annex F specifies.
    #[test]
    fn special_values_follow_annex_f() {
        let cases = [
            (0.0f32, 1.0f32, 0.0f64),
            (-0.0, 1.0, -0.0),
            (0.0, 0.0, 0.0),
            (-0.0, 0.0, -0.0),
            (0.0, -0.0, PI),
            (-0.0, -0.0, -PI),
            (0.0, -1.0, PI),
            (-0.0, -1.0, -PI),
            (1.0, 0.0, FRAC_PI_2),
            (-1.0, -0.0, -FRAC_PI_2),
            (f32::INFINITY, 1.0, FRAC_PI_2),
            (f32::NEG_INFINITY, -1.0, -FRAC_PI_2),
            (f32::INFINITY, f32::INFINITY, FRAC_PI_4),
            (f32::INFINITY, f32::NEG_INFINITY, FRAC_3PI_4),
            (f32::NEG_INFINITY, f32::NEG_INFINITY, -FRAC_3PI_4),
            (1.0, f32::INFINITY, 0.0),
            (-1.0, f32::INFINITY, -0.0),
            (1.0, f32::NEG_INFINITY, PI),
            (-1.0, f32::NEG_INFINITY, -PI),
        ];
        for (y, x, expected) in cases {
            assert_eq!(atan2(y, x).to_bits(), expected.to_bits(), "atan2({y}, {x})");
        }
        assert!(atan2(f32::NAN, 1.0).is_nan());
        assert!(atan2(1.0, f32::NAN).is_nan());
        assert_eq!(FRAC_3PI_4.to_bits(), (3.0 * FRAC_PI_4).to_bits());
    }

    /// Against the platform's f64 `atan2` on random bit patterns and
    /// pixel-scale offsets: within 3 ulp (each is within about 1.5 of the
    /// exact value).
    #[test]
    fn stays_close_to_the_platform_atan2() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        for _ in 0..1_000_000 {
            let bits = next();
            let (y, x) = (
                f32::from_bits(bits as u32),
                f32::from_bits((bits >> 32) as u32),
            );
            let pixel_y = ((next() >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 1000.0;
            let pixel_x = ((next() >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 1000.0;
            for (y, x) in [(y, x), (pixel_y, pixel_x)] {
                let got = atan2(y, x);
                let platform = f64::from(y).atan2(f64::from(x));
                if got.is_nan() || platform.is_nan() {
                    assert!(got.is_nan() && platform.is_nan(), "atan2({y:e}, {x:e})");
                } else {
                    assert!(
                        ulps(got, platform) <= 3,
                        "atan2({y:e}, {x:e}): {got:e} vs {platform:e}"
                    );
                }
            }
        }
    }
}
