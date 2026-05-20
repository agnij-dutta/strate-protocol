//! Explicit rounding direction.
//!
//! Pendle's Solidity reference enforces a discipline: every multiply or
//! divide call site declares whether it rounds toward zero, away from
//! zero, or to nearest (banker's). This eliminates the most common class
//! of off-by-one drift bugs in AMMs and yield accounting.
//!
//! In contract code, prefer `mul_wad_up` / `mul_wad_down` directly rather
//! than passing this enum, because the inlined name is auditor-readable.
//! [`RoundingMode`] exists for the rare branches where the direction is
//! data-dependent (for example, mint-vs-burn paths sharing a helper).

/// Explicit rounding direction for fixed-point operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    /// Round toward negative infinity (a.k.a. floor).
    ///
    /// Used for **assets out** of the protocol — under-counting always
    /// favors the protocol's solvency.
    Floor,
    /// Round toward positive infinity (a.k.a. ceiling).
    ///
    /// Used for **liabilities, fees, debt** — over-counting always
    /// favors the protocol's solvency.
    Ceiling,
    /// Banker's rounding (round half to even).
    ///
    /// Used for **value-neutral conversions** like internal index
    /// updates where neither side should systematically gain.
    HalfEven,
}

impl RoundingMode {
    /// Returns `true` if this mode rounds toward `+inf` on a tie or
    /// non-zero remainder. Useful for sharing a `muldiv` implementation
    /// across all three modes.
    #[must_use]
    pub fn is_ceiling(self) -> bool {
        matches!(self, RoundingMode::Ceiling)
    }

    /// Returns `true` if this mode rounds toward `-inf` on a tie or
    /// non-zero remainder.
    #[must_use]
    pub fn is_floor(self) -> bool {
        matches!(self, RoundingMode::Floor)
    }
}
