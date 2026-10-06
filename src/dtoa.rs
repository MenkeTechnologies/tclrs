//! The shortest digit string Tcl prints for a double.
//!
//! A port of `TclDoubleDigits` in `TCL_DD_SHORTEST` mode
//! (`generic/tclStrToD.c`), the digit generator behind `Tcl_PrintDouble`. It is
//! Steele and White's free-format algorithm, and two of its decisions differ
//! from the shortest round-trip conversion Rust's formatter (and most others)
//! implement, so the digits are not interchangeable:
//!
//! * **Ties go to the even digit.** When the last digit could be rounded either
//!   way with both results inside the rounding interval, Tcl keeps the one
//!   nearer the exact value and, at an exact tie, the even one:
//!   `expr {1e15+0.3}` is `1000000000000000.2` (the double is
//!   1000000000000000.25).
//! * **A power of two takes the wider tolerance below it.** For a significand
//!   that is a power of two the gap to the next double down is half the gap up.
//!   `ShorteningInt64Conversion` tests the remainder below the value against the
//!   *upper* half-gap and the one above against the *lower* one, so digits that
//!   read back as the next double down are accepted: `expr {2.0**64}` is
//!   `1.844674407370955e+19`, which is not equal to 2.0**64.
//!
//! The three conversion routines `TclDoubleDigits` chooses between
//! (`ShorteningInt64Conversion`, `ShorteningBignumConversionPowD`,
//! `ShorteningBignumConversion`) compute the same exact quantities with
//! different arithmetic; the one place they differ — the bignum routine scales
//! `mplus` by 2^10 instead of 10 once `s5` is exhausted — is never reached in
//! shortest mode, which stops within 17 digits while `s5` is the decimal
//! exponent of a number of at least 1e24. So one exact loop serves all three:
//! over `u128` when the scaled values fit, over `BigUint` otherwise.

use num_bigint::BigUint;
use num_traits::ToPrimitive;
use std::ops::{Add, Div, Mul, Rem, Sub};

/// The digits of `|f|` and the power of ten of the first one: `f` is
/// `0.d1d2… × 10^(k+1)`, i.e. `d1.d2… × 10^k`. `f` must be finite and nonzero.
pub(crate) fn shortest(f: f64) -> (String, i32) {
    let bits = f.abs().to_bits();
    let de = ((bits >> 52) & 0x7FF) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let even = bits & 1 == 0;
    // `DoubleToExpAndSig`: the significand with its trailing zeros shifted
    // out, its bit count, and the power of two it is scaled by.
    let (mut bw, denorm) = if de != 0 {
        (frac | (1u64 << 52), false)
    } else {
        (frac, true)
    };
    let tz = bw.trailing_zeros() as i32;
    bw >>= tz;
    let (be, bbits) = if denorm {
        (tz - 1023 - 52 + 1, 64 - bw.leading_zeros() as i32)
    } else {
        (tz + (de - 1023) - 52, 53 - tz)
    };
    // Half a unit in the last place, as a power of two.
    let half = if denorm { -1075 } else { be + bbits - 54 };
    // In the special case where bw == 1, the nearest double on the low side is
    // a quarter ulp away: `m-` is halved and `m+` is not.
    let (mplus2, mminus2) = if !denorm && bw == 1 {
        (half, half - 1)
    } else {
        (half, half)
    };
    let k = floor_log10(f.abs());

    // Every quantity is n·2^p2·5^p5 with p2, p5 possibly negative: scale them
    // all by the smallest exponents so that each is an integer.
    //   b = bw·2^be,  S = 2^k·5^k,  m+ = 2^mplus2,  m- = 2^mminus2.
    let min2 = be.min(k).min(mplus2).min(mminus2);
    let min5 = 0.min(k);
    let term = |n: u64, p2: i32, p5: i32| -> BigUint {
        (BigUint::from(n) << ((p2 - min2) as usize)) * BigUint::from(5u32).pow((p5 - min5) as u32)
    };
    let b = term(bw, be, 0);
    let s = term(1, k, k);
    let mplus = term(1, mplus2, 0);
    let mminus = term(1, mminus2, 0);

    if s.bits() < 120 {
        if let (Some(b), Some(s), Some(mp), Some(mm)) =
            (b.to_u128(), s.to_u128(), mplus.to_u128(), mminus.to_u128())
        {
            return generate(b, s, mp, mm, even, k);
        }
    }
    generate(b, s, mplus, mminus, even, k)
}

/// `floor(log10(x))`, exactly. `ApproximateLog10` and `BetterLog10` only
/// estimate it, and the conversion corrects an estimate one too high by
/// rescaling; the digits come out the same either way.
fn floor_log10(x: f64) -> i32 {
    let mut k = x.log10().floor() as i32;
    // `log10` may be off by one at an exact power of ten; settle it exactly.
    let exact = |k: i32| -> std::cmp::Ordering {
        // Compare x with 10^k as rationals.
        let bits = x.to_bits();
        let de = ((bits >> 52) & 0x7FF) as i32;
        let frac = bits & ((1u64 << 52) - 1);
        let (m, e) = if de != 0 {
            (frac | (1u64 << 52), de - 1075)
        } else {
            (frac, -1074)
        };
        let lhs = BigUint::from(m) << (e.max(0) as usize);
        let lhs = lhs * BigUint::from(10u32).pow((-k).max(0) as u32);
        let rhs = BigUint::from(10u32).pow(k.max(0) as u32) << ((-e).max(0) as usize);
        lhs.cmp(&rhs)
    };
    while exact(k) == std::cmp::Ordering::Less {
        k -= 1;
    }
    while exact(k + 1) != std::cmp::Ordering::Less {
        k += 1;
    }
    k
}

/// The digit loop of `ShorteningInt64Conversion`, over exact integers:
/// `b / s` is the value scaled into [1, 10), `mp / s` and `mm / s` the
/// tolerances above and below it.
fn generate<T>(mut b: T, s: T, mut mp: T, mut mm: T, even: bool, mut k: i32) -> (String, i32)
where
    T: Clone
        + Ord
        + From<u32>
        + ToPrimitive
        + Add<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Div<Output = T>
        + Rem<Output = T>,
{
    let ten = T::from(10);
    let two = T::from(2);
    let mut out: Vec<u8> = Vec::with_capacity(18);
    loop {
        let mut digit = (b.clone() / s.clone()).to_u32().unwrap_or(0);
        b = b % s.clone();
        // Does the current digit put us on the low side of the exact value but
        // within roundoff of being exact? (Tested against m+, as Tcl does.)
        if b < mp || (b == mp && even) {
            let twice = b.clone() * two.clone();
            if twice > s || (twice == s && digit & 1 == 1) {
                digit += 1;
                if digit == 10 {
                    out.push(b'9');
                    bump_up(&mut out, &mut k);
                    break;
                }
            }
            out.push(b'0' + digit as u8);
            break;
        }
        // Does one plus the current digit put us within roundoff? (Against m-.)
        let up = b.clone() + mm.clone();
        if up > s || (up == s && even) {
            if digit == 9 {
                out.push(b'9');
                bump_up(&mut out, &mut k);
                break;
            }
            out.push(b'0' + digit as u8 + 1);
            break;
        }
        out.push(b'0' + digit as u8);
        b = b * ten.clone();
        mp = mp * ten.clone();
        mm = mm * ten.clone();
    }
    (String::from_utf8(out).unwrap_or_default(), k)
}

/// `BumpUp`: add one to the last digit, carrying through trailing nines.
fn bump_up(out: &mut Vec<u8>, k: &mut i32) {
    while out.last() == Some(&b'9') {
        out.pop();
    }
    match out.last_mut() {
        Some(d) => *d += 1,
        None => {
            *k += 1;
            out.push(b'1');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shortest;

    /// Values measured against tclsh 9.0.4's `Tcl_PrintDouble`.
    #[test]
    fn digits_match_tcl() {
        assert_eq!(shortest(1e15 + 0.3), ("10000000000000002".into(), 15));
        assert_eq!(shortest(2f64.powi(64)), ("1844674407370955".into(), 19));
        assert_eq!(shortest(2f64.powi(65)), ("368934881474191".into(), 19));
        assert_eq!(shortest(0.1), ("1".into(), -1));
        assert_eq!(shortest(1.5), ("15".into(), 0));
        assert_eq!(shortest(5e-324), ("5".into(), -324));
        assert_eq!(
            shortest(1.7976931348623157e308),
            ("17976931348623157".into(), 308)
        );
    }
}
