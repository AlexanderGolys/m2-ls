//! Exact numbers of the `ZZ` and `QQ` classes, with the arithmetic Macaulay2
//! evaluates natively. Every operation is checked: a result that overflows, or
//! that Macaulay2 defines only by convention (a zero divisor), is absent rather
//! than guessed.

use std::cmp::Ordering;
use std::fmt::{self, Display, Formatter};

/// Which of the two exact number classes a value belongs to. `4/2` is a `QQ`
/// value although it equals `2`, which Macaulay2 tells apart under `===`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberClass {
    /// `ZZ`.
    Integer,
    /// `QQ`.
    Rational,
}

/// An exact number: a reduced fraction with a positive denominator and its
/// class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactNumber {
    /// Numerator of the reduced fraction.
    numerator: i128,
    /// Positive denominator of the reduced fraction.
    denominator: i128,
    /// The class Macaulay2 gives the value.
    class: NumberClass,
}

/// The greatest common divisor of two integers, never negative.
fn gcd(left: i128, right: i128) -> Option<i128> {
    let (mut left, mut right) = (left.checked_abs()?, right.checked_abs()?);
    while right != 0 {
        (left, right) = (right, left % right);
    }
    Some(left)
}

impl ExactNumber {
    /// The integer `value`.
    pub fn integer(value: i128) -> Self {
        Self {
            numerator: value,
            denominator: 1,
            class: NumberClass::Integer,
        }
    }

    /// The fraction `numerator/denominator` as Macaulay2's `/` builds it: a
    /// `QQ` value, absent for a zero denominator.
    pub fn quotient(numerator: i128, denominator: i128) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let divisor = gcd(numerator, denominator)?;
        let sign = denominator.signum();
        Some(Self {
            numerator: sign.checked_mul(numerator / divisor)?,
            denominator: sign.checked_mul(denominator / divisor)?,
            class: NumberClass::Rational,
        })
    }

    /// The value when it is an integer of the `ZZ` class.
    pub fn as_integer(self) -> Option<i128> {
        (self.class == NumberClass::Integer).then_some(self.numerator)
    }

    /// The class of the result of an operation on `self` and `other` that stays
    /// in `ZZ` for two integers.
    fn joint_class(self, other: Self) -> NumberClass {
        if self.class == NumberClass::Integer && other.class == NumberClass::Integer {
            NumberClass::Integer
        } else {
            NumberClass::Rational
        }
    }

    /// The same value read in `class`, which keeps the fraction.
    fn in_class(self, class: NumberClass) -> Self {
        Self { class, ..self }
    }

    /// `self + other`.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let numerator = self
            .numerator
            .checked_mul(other.denominator)?
            .checked_add(other.numerator.checked_mul(self.denominator)?)?;
        Self::quotient(numerator, self.denominator.checked_mul(other.denominator)?)
            .map(|sum| sum.in_class(self.joint_class(other)))
    }

    /// `-self`.
    pub fn checked_neg(self) -> Option<Self> {
        Some(Self {
            numerator: self.numerator.checked_neg()?,
            ..self
        })
    }

    /// `self - other`.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.checked_add(other.checked_neg()?)
    }

    /// `self * other`.
    pub fn checked_mul(self, other: Self) -> Option<Self> {
        Self::quotient(
            self.numerator.checked_mul(other.numerator)?,
            self.denominator.checked_mul(other.denominator)?,
        )
        .map(|product| product.in_class(self.joint_class(other)))
    }

    /// `self / other`, always a `QQ` value; absent for a zero divisor.
    pub fn checked_div(self, other: Self) -> Option<Self> {
        Self::quotient(
            self.numerator.checked_mul(other.denominator)?,
            self.denominator.checked_mul(other.numerator)?,
        )
    }

    /// `self ^ exponent`; a negative exponent gives a `QQ` value, and `0^(-n)`
    /// is absent.
    pub fn checked_pow(self, exponent: i128, largest_exponent: u32) -> Option<Self> {
        let magnitude = u32::try_from(exponent.unsigned_abs()).ok()?;
        if magnitude > largest_exponent {
            return None;
        }
        let power = Self::quotient(
            self.numerator.checked_pow(magnitude)?,
            self.denominator.checked_pow(magnitude)?,
        )?;
        if exponent >= 0 {
            Some(power.in_class(self.class))
        } else {
            Self::integer(1).checked_div(power)
        }
    }

    /// The Euclidean quotient `self // divisor` of two integers: the
    /// remainder stays in `0 .. |divisor|`. Absent for a zero divisor, where
    /// Macaulay2 answers by convention.
    pub fn euclidean_quotient(self, divisor: Self) -> Option<Self> {
        let (dividend, divisor) = (self.as_integer()?, divisor.as_integer()?);
        let remainder = Self::euclidean_remainder_of(dividend, divisor)?;
        Some(Self::integer(dividend.checked_sub(remainder)? / divisor))
    }

    /// The Euclidean remainder `self % divisor` of two integers, in
    /// `0 .. |divisor|`.
    pub fn euclidean_remainder(self, divisor: Self) -> Option<Self> {
        Self::euclidean_remainder_of(self.as_integer()?, divisor.as_integer()?).map(Self::integer)
    }

    /// The remainder of `dividend` by `divisor` in `0 .. |divisor|`.
    fn euclidean_remainder_of(dividend: i128, divisor: i128) -> Option<i128> {
        let modulus = divisor.checked_abs().filter(|modulus| *modulus != 0)?;
        Some(dividend.rem_euclid(modulus))
    }

    /// How `self` compares with `other`, absent when the comparison overflows.
    pub fn compare(self, other: Self) -> Option<Ordering> {
        Some(
            self.numerator
                .checked_mul(other.denominator)?
                .cmp(&other.numerator.checked_mul(self.denominator)?),
        )
    }

    /// Macaulay2's `===`: equal value and equal class.
    pub fn is_identical(self, other: Self) -> bool {
        self == other
    }

    /// `abs self`.
    pub fn checked_abs(self) -> Option<Self> {
        Some(Self {
            numerator: self.numerator.checked_abs()?,
            ..self
        })
    }

    /// `gcd(self, other)` of two integers.
    pub fn gcd(self, other: Self) -> Option<Self> {
        gcd(self.as_integer()?, other.as_integer()?).map(Self::integer)
    }

    /// `lcm(self, other)` of two integers.
    pub fn lcm(self, other: Self) -> Option<Self> {
        let (left, right) = (self.as_integer()?, other.as_integer()?);
        if left == 0 || right == 0 {
            return Some(Self::integer(0));
        }
        let divisor = gcd(left, right)?;
        left.checked_mul(right / divisor)
            .and_then(i128::checked_abs)
            .map(Self::integer)
    }

    /// `binomial(self, other)` of two non-negative integers.
    pub fn binomial(self, other: Self) -> Option<Self> {
        let (total, chosen) = (self.as_integer()?, other.as_integer()?);
        if total < 0 || chosen < 0 {
            return None;
        }
        if chosen > total {
            return Some(Self::integer(0));
        }
        let chosen = chosen.min(total - chosen);
        let mut value: i128 = 1;
        for step in 1..=chosen {
            value = value.checked_mul(total - chosen + step)? / step;
        }
        Some(Self::integer(value))
    }
}

impl Display for ExactNumber {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if self.denominator == 1 {
            write!(formatter, "{}", self.numerator)
        } else {
            write!(formatter, "{}/{}", self.numerator, self.denominator)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExactNumber;

    /// An integer `ZZ` value.
    fn zz(value: i128) -> ExactNumber {
        ExactNumber::integer(value)
    }

    /// The `QQ` value `numerator/denominator`.
    fn qq(numerator: i128, denominator: i128) -> ExactNumber {
        ExactNumber::quotient(numerator, denominator).expect("a non-zero denominator")
    }

    /// `//` and `%` are Euclidean in Macaulay2: the remainder lies in
    /// `0 .. |divisor|` whatever the signs, as probed on 1.26.06.
    #[test]
    fn division_with_remainder_is_euclidean() {
        let cases = [(-7, 3, -3, 2), (7, -3, -2, 1), (-7, -3, 3, 2), (7, 3, 2, 1)];
        for (dividend, divisor, quotient, remainder) in cases {
            assert_eq!(
                zz(dividend).euclidean_quotient(zz(divisor)),
                Some(zz(quotient)),
                "{dividend} // {divisor}"
            );
            assert_eq!(
                zz(dividend).euclidean_remainder(zz(divisor)),
                Some(zz(remainder)),
                "{dividend} % {divisor}"
            );
        }
        assert_eq!(
            zz(1).euclidean_quotient(zz(0)),
            None,
            "zero divisors are conventions"
        );
        assert_eq!(qq(7, 2).euclidean_remainder(zz(2)), None);
    }

    /// `QQ` stays `QQ` after an operation that lands on an integer, and
    /// `===` tells the classes apart.
    #[test]
    fn classes_follow_macaulay2() {
        let half = qq(1, 2);
        assert_eq!(
            half.checked_mul(zz(2)).map(|value| value.to_string()),
            Some("1".into())
        );
        assert!(!half
            .checked_mul(zz(2))
            .is_some_and(|value| value.is_identical(zz(1))));
        assert!(qq(4, 2).compare(zz(2)).is_some_and(|order| order.is_eq()));
        assert!(!qq(4, 2).is_identical(zz(2)));
        assert_eq!(qq(6, -4).to_string(), "-3/2");
        assert_eq!(zz(2).checked_pow(10, 4096), Some(zz(1024)));
        assert_eq!(zz(2).checked_pow(-2, 4096), Some(qq(1, 4)));
        assert_eq!(zz(-2).checked_pow(-3, 4096), Some(qq(-1, 8)));
        assert_eq!(zz(0).checked_pow(0, 4096), Some(zz(1)));
        assert_eq!(zz(0).checked_pow(-1, 4096), None);
        assert_eq!(zz(2).checked_pow(5000, 4096), None, "a power stays cheap");
        assert_eq!(
            zz(10).checked_pow(40, 4096),
            None,
            "overflow is not guessed"
        );
        assert_eq!(zz(1).checked_div(zz(0)), None);
    }

    /// `gcd`, `lcm` and `binomial` as Macaulay2 computes them.
    #[test]
    fn number_theoretic_functions_agree_with_macaulay2() {
        assert_eq!(zz(-4).gcd(zz(6)), Some(zz(2)));
        assert_eq!(zz(0).gcd(zz(0)), Some(zz(0)));
        assert_eq!(zz(-4).lcm(zz(6)), Some(zz(12)));
        assert_eq!(zz(0).lcm(zz(3)), Some(zz(0)));
        assert_eq!(zz(10).binomial(zz(3)), Some(zz(120)));
        assert_eq!(zz(5).binomial(zz(7)), Some(zz(0)));
        assert_eq!(zz(5).binomial(zz(0)), Some(zz(1)));
        assert_eq!(
            zz(-1).binomial(zz(2)),
            None,
            "the generalized binomial is not folded"
        );
        assert_eq!(zz(60).binomial(zz(30)), Some(zz(118264581564861424)));
    }
}
