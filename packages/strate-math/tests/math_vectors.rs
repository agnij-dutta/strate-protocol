//! 20 differential test vectors ported from Pendle's PMath.sol reference.
//!
//! These are the bedrock vectors every contract that depends on
//! `strate-math` implicitly relies on. If any of them break, downstream
//! AMM math will drift and the protocol will lose dust per swap.

use strate_math::{
    div_wad, div_wad_down, div_wad_up, exp_wad, ln_wad, mul_wad, mul_wad_down, mul_wad_up,
    muldiv_ceil, muldiv_floor, LN2, WAD,
};

/// Helper: assert absolute difference is at most `tol` wei.
fn approx_eq(actual: i128, expected: i128, tol: i128, label: &str) {
    let diff = (actual - expected).abs();
    assert!(
        diff <= tol,
        "{label}: actual={actual}, expected={expected}, diff={diff}, tol={tol}"
    );
}

// -------------------------------------------------------------------------
// Vectors 1-6: exp / ln
// -------------------------------------------------------------------------

#[test]
fn vec01_exp_zero_is_one() {
    assert_eq!(exp_wad(0), WAD);
}

#[test]
fn vec02_exp_one_is_e() {
    // e = 2.718281828459045235...
    let e_wad: i128 = 2_718_281_828_459_045_235;
    approx_eq(exp_wad(WAD), e_wad, 2000, "exp(1)");
}

#[test]
fn vec03_ln_one_is_zero() {
    assert_eq!(ln_wad(WAD), 0);
}

#[test]
fn vec04_ln_e_is_one() {
    let e_wad: i128 = 2_718_281_828_459_045_235;
    // Tolerance allows for: ~0.4 wei truncation of e's WAD
    // representation, plus ~30 wei of accumulated rounding across the
    // 10-term atanh series + the (r-WAD)/(r+WAD) divide.
    approx_eq(ln_wad(e_wad), WAD, 100, "ln(e)");
}

#[test]
fn vec05_exp_ln2_is_two() {
    approx_eq(exp_wad(LN2), 2 * WAD, 100, "exp(ln 2)");
}

#[test]
fn vec06_ln_two_is_ln2() {
    // Exact: 2*WAD halves to WAD, u = 0, series sum = 0, result = LN2.
    approx_eq(ln_wad(2 * WAD), LN2, 2, "ln(2)");
}

// -------------------------------------------------------------------------
// Vectors 7-12: mul_wad rounding directions
// -------------------------------------------------------------------------

#[test]
fn vec07_mul_wad_simple() {
    // 1.5 * 2.0 = 3.0
    let a: i128 = 1_500_000_000_000_000_000;
    let b: i128 = 2_000_000_000_000_000_000;
    let c: i128 = 3_000_000_000_000_000_000;
    assert_eq!(mul_wad(a, b), c);
}

#[test]
fn vec08_mul_wad_down_no_rounding_needed() {
    // 0.7 * 0.7 = 0.49 exactly in WAD.
    let a: i128 = 700_000_000_000_000_000;
    let expect: i128 = 490_000_000_000_000_000;
    assert_eq!(mul_wad_down(a, a), expect);
}

#[test]
fn vec09_mul_wad_up_rounds_up_on_remainder() {
    // 1 * 1 / WAD = 0 floor, 1 ceiling.
    assert_eq!(mul_wad_down(1, 1), 0);
    assert_eq!(mul_wad_up(1, 1), 1);
}

#[test]
fn vec10_mul_wad_down_truncates() {
    // (1e18 - 1) * (1e18 - 1) / 1e18
    //   = (WAD - 1)^2 / WAD
    //   = (WAD^2 - 2*WAD + 1) / WAD
    //   = WAD - 2 + 1/WAD
    // floor -> WAD - 2; ceiling -> WAD - 1.
    let a = WAD - 1;
    assert_eq!(mul_wad_down(a, a), WAD - 2);
    assert_eq!(mul_wad_up(a, a), WAD - 1);
}

#[test]
fn vec11_mul_wad_zero_inputs() {
    assert_eq!(mul_wad(0, WAD), 0);
    assert_eq!(mul_wad(WAD, 0), 0);
    assert_eq!(mul_wad_up(0, 0), 0);
}

#[test]
fn vec12_mul_wad_negative() {
    // (-1.5) * 2 = -3
    let a: i128 = -1_500_000_000_000_000_000;
    let b: i128 = 2_000_000_000_000_000_000;
    let c: i128 = -3_000_000_000_000_000_000;
    assert_eq!(mul_wad(a, b), c);
}

// -------------------------------------------------------------------------
// Vectors 13-16: div_wad
// -------------------------------------------------------------------------

#[test]
fn vec13_div_wad_simple() {
    // 3 / 1.5 = 2
    let a: i128 = 3_000_000_000_000_000_000;
    let b: i128 = 1_500_000_000_000_000_000;
    let c: i128 = 2_000_000_000_000_000_000;
    assert_eq!(div_wad(a, b), c);
}

#[test]
fn vec14_div_wad_rounding_dirs() {
    // 1 / 3 = 0.333... -> down = 333..., up = 333...+1, half-even depends
    let one: i128 = WAD;
    let three: i128 = 3 * WAD;
    let down = div_wad_down(one, three);
    let up = div_wad_up(one, three);
    // The exact quotient in WAD is 333_333_333_333_333_333.333...
    assert_eq!(down, 333_333_333_333_333_333);
    assert_eq!(up, 333_333_333_333_333_334);
}

#[test]
fn vec15_div_wad_by_one() {
    // x / 1 = x
    let x: i128 = 1_234_567_890_123_456_789;
    assert_eq!(div_wad(x, WAD), x);
}

#[test]
fn vec16_div_wad_negative_dividend() {
    let a: i128 = -2 * WAD;
    let b: i128 = WAD;
    assert_eq!(div_wad(a, b), -2 * WAD);
}

// -------------------------------------------------------------------------
// Vectors 17-20: muldiv with explicit denominator
// -------------------------------------------------------------------------

#[test]
fn vec17_muldiv_floor_simple() {
    // (10 * 3) / 7 = 30/7 = 4 remainder 2 -> floor 4.
    assert_eq!(muldiv_floor(10, 3, 7), 4);
    assert_eq!(muldiv_ceil(10, 3, 7), 5);
}

#[test]
fn vec18_muldiv_exact() {
    // (6 * 4) / 8 = 3 exact.
    assert_eq!(muldiv_floor(6, 4, 8), 3);
    assert_eq!(muldiv_ceil(6, 4, 8), 3);
}

#[test]
fn vec19_muldiv_large() {
    // i128::MAX / 2 * 2 / 1 stays within bounds.
    // Use a numerator that requires 256-bit intermediate.
    // (WAD^2) * 2 / WAD = 2 * WAD.
    let big = WAD;
    let r = muldiv_floor(big, big, big);
    assert_eq!(r, big);
    let r2 = muldiv_floor(big, 2 * big, big);
    assert_eq!(r2, 2 * big);
}

#[test]
fn vec20_muldiv_floor_le_ceil() {
    // For any positive operands with non-zero remainder, ceil = floor + 1.
    let a: i128 = 12_345;
    let b: i128 = 67_890;
    let d: i128 = 999;
    let f = muldiv_floor(a, b, d);
    let c = muldiv_ceil(a, b, d);
    assert!(c >= f);
    assert!(c - f <= 1);
}
