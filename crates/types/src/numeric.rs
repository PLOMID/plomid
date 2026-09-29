//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! Variable-precision decimal arithmetic, PostgreSQL `numeric` compatible.
//!
//! Representation: a 128-bit signed mantissa plus a scale (digits after the
//! decimal point). Addition, subtraction, multiplication and comparison are
//! exact; division rounds to the larger operand scale plus 4, like PostgreSQL.

use std::cmp::Ordering;
use std::fmt;

/// A PostgreSQL-style numeric value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numeric {
    /// Signed mantissa: the value equals `mantissa / 10^scale`.
    pub mantissa: i128,
    /// Digits after the decimal point.
    pub scale: u16,
}

fn numeric_err<T>(msg: &str) -> Result<T, String> {
    Err(msg.to_string())
}

impl Numeric {
    /// Maximum mantissa magnitude (~38 decimal digits).
    pub const MAX: i128 = 99_999_999_999_999_999_999_999_999_999_999_999_999;

    /// Parses a numeric literal: optional sign, digits, fraction, exponent.
    pub fn parse(input: &str) -> Result<Self, String> {
        let text = input.trim();
        if text.is_empty() {
            return numeric_err("invalid input syntax for type numeric");
        }
        let mut chars = text.chars().peekable();
        let mut negative = false;
        match chars.peek() {
            Some('+') => {
                chars.next();
            }
            Some('-') => {
                negative = true;
                chars.next();
            }
            _ => {}
        }
        let mut int_digits = String::new();
        let mut frac_digits = String::new();
        while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
            int_digits.push(chars.next().expect("peeked"));
        }
        if matches!(chars.peek(), Some('.')) {
            chars.next();
            while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
                frac_digits.push(chars.next().expect("peeked"));
            }
        }
        let mut exponent: i32 = 0;
        if matches!(chars.peek(), Some('e') | Some('E')) {
            chars.next();
            let mut exp_text = String::new();
            if matches!(chars.peek(), Some('+') | Some('-')) {
                exp_text.push(chars.next().expect("peeked"));
            }
            let mut any = false;
            while let Some(&c) = chars.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                exp_text.push(c);
                any = true;
                chars.next();
            }
            if !any {
                return numeric_err("invalid input syntax for type numeric");
            }
            exponent = exp_text
                .parse()
                .map_err(|_| "exponent overflow".to_string())?;
        }
        if chars.peek().is_some() || (int_digits.is_empty() && frac_digits.is_empty()) {
            return numeric_err("invalid input syntax for type numeric");
        }
        let mut digits = format!("{int_digits}{frac_digits}");
        let mut scale = frac_digits.len() as i32 - exponent;
        if scale < 0 {
            digits.push_str(&"0".repeat(-scale as usize));
            scale = 0;
        }
        while scale > 0 && digits.ends_with('0') {
            digits.pop();
            scale -= 1;
        }
        let trimmed = digits.trim_start_matches('0');
        let mantissa: i128 = if trimmed.is_empty() {
            0
        } else {
            trimmed
                .parse()
                .or_else(|_| numeric_err("numeric value out of range"))?
        };
        let mantissa = if negative { -mantissa } else { mantissa };
        Ok(Self::new(mantissa, scale.max(0) as u16))
    }

    /// Creates a numeric from a mantissa and scale.
    #[must_use]
    pub const fn new(mantissa: i128, scale: u16) -> Self {
        Self { mantissa, scale }
    }

    /// Creates a numeric from an integer.
    #[must_use]
    pub const fn from_i64(value: i64) -> Self {
        Self {
            mantissa: value as i128,
            scale: 0,
        }
    }

    /// Strips trailing fractional zeros (e.g. 27.5000 -> 27.5, 21.0000 -> 21).
    /// Used for division results like AVG where PostgreSQL renders the
    /// canonical numeric form rather than the raw computation scale.
    #[must_use]
    pub fn normalize(mut self) -> Self {
        while self.scale > 0 && self.mantissa % 10 == 0 {
            self.mantissa /= 10;
            self.scale -= 1;
        }
        if self.mantissa == 0 {
            self.scale = 0;
        }
        self
    }

    /// Creates a numeric from an f64 (best-effort decimal representation).
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        if !value.is_finite() {
            return Self::new(0, 0);
        }
        let scale = 15 - value.abs().log10().floor().max(0.0) as i32;
        let scale = scale.clamp(0, 15) as u16;
        let mantissa = (value * 10_f64.powi(i32::from(scale))).round() as i128;
        Self { mantissa, scale }
    }

    /// Converts to f64 (may lose precision).
    #[must_use]
    pub fn to_f64(self) -> f64 {
        self.mantissa as f64 / 10_f64.powi(i32::from(self.scale))
    }

    /// Converts to i64 when the value is integral and in range.
    #[must_use]
    pub fn to_i64(self) -> Option<i64> {
        let factor = 10_i128.pow(u32::from(self.scale));
        if self.mantissa.rem_euclid(factor) != 0 {
            return None;
        }
        i64::try_from(self.mantissa / factor).ok()
    }
}

// Fallible Result-returning arithmetic methods (names mirror the ops traits).
#[allow(clippy::should_implement_trait)]
impl Numeric {
    /// Rescales to the given scale (truncating when shrinking).
    #[must_use]
    pub fn rescale(mut self, scale: u16) -> Self {
        match scale.cmp(&self.scale) {
            Ordering::Equal => self,
            Ordering::Greater => {
                let diff = u32::from(scale - self.scale);
                self.mantissa *= 10_i128.pow(diff);
                self.scale = scale;
                self
            }
            Ordering::Less => {
                let diff = u32::from(self.scale - scale);
                self.mantissa /= 10_i128.pow(diff);
                self.scale = scale;
                self
            }
        }
    }

    /// Rescales to the given scale with PostgreSQL-compatible rounding (half away from zero).
    #[must_use]
    pub fn round_to_scale(self, scale: u16) -> Self {
        if scale >= self.scale {
            // Need to add precision - use PostgreSQL rounding
            let diff = u32::from(scale - self.scale);
            let factor = 10_i128.pow(diff);
            // Round half away from zero: add factor/2 before dividing
            let sign = if self.mantissa >= 0 { 1 } else { -1 };
            let abs_mantissa = self.mantissa.abs();
            let rounded = (abs_mantissa + factor / 2) / factor * factor;
            let result_mantissa = if sign >= 0 { rounded } else { -rounded };
            return Self::new(result_mantissa, scale);
        }

        // Scale is smaller, need to round
        let diff = u32::from(self.scale - scale);
        let factor = 10_i128.pow(diff);
        // Round half away from zero
        let sign = if self.mantissa >= 0 { 1 } else { -1 };
        let abs_mantissa = self.mantissa.abs();
        let rounded = (abs_mantissa + factor / 2) / factor * factor;
        let result_mantissa = if sign >= 0 { rounded } else { -rounded };
        Self::new(result_mantissa / factor, scale)
    }

    /// Aligns two numerics to the larger scale.
    fn align(a: Self, b: Self) -> (i128, i128, u16) {
        if a.scale >= b.scale {
            (a.mantissa, b.rescale(a.scale).mantissa, a.scale)
        } else {
            (a.rescale(b.scale).mantissa, b.mantissa, b.scale)
        }
    }

    /// Checked addition.
    pub fn add(self, other: Self) -> Result<Self, String> {
        let (a, b, scale) = Self::align(self, other);
        Ok(Self::new(
            a.checked_add(b).ok_or("numeric value out of range")?,
            scale,
        ))
    }

    /// Checked subtraction.
    pub fn sub(self, other: Self) -> Result<Self, String> {
        let (a, b, scale) = Self::align(self, other);
        Ok(Self::new(
            a.checked_sub(b).ok_or("numeric value out of range")?,
            scale,
        ))
    }

    /// Checked multiplication (exact).
    pub fn mul(self, other: Self) -> Result<Self, String> {
        let mantissa = self
            .mantissa
            .checked_mul(other.mantissa)
            .ok_or("numeric value out of range")?;
        Ok(Self::new(mantissa, self.scale + other.scale))
    }

    /// Division, rounding to the larger scale plus 4 fractional digits.
    pub fn div(self, other: Self) -> Result<Self, String> {
        if other.mantissa == 0 {
            return numeric_err("division by zero");
        }
        let scale = self.scale.max(other.scale + 4);
        let a = self.rescale(scale + other.scale).mantissa;
        let mantissa = a
            .checked_div(other.mantissa)
            .ok_or("numeric value out of range")?;
        Ok(Self::new(mantissa, scale))
    }

    /// Remainder (mod).
    pub fn rem(self, other: Self) -> Result<Self, String> {
        if other.mantissa == 0 {
            return numeric_err("division by zero");
        }
        let (a, b, scale) = Self::align(self, other);
        Ok(Self::new(a.rem_euclid(b.abs()) * a.signum(), scale))
    }

    /// Negation.
    #[must_use]
    pub fn neg(self) -> Self {
        Self::new(-self.mantissa, self.scale)
    }

    /// Absolute value.
    #[must_use]
    pub fn abs(self) -> Self {
        Self::new(self.mantissa.abs(), self.scale)
    }

    /// True when the value is zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.mantissa == 0
    }
}

impl PartialOrd for Numeric {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Numeric {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b, _) = Self::align(self.clone(), other.clone());
        a.cmp(&b)
    }
}

impl fmt::Display for Numeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let negative = self.mantissa < 0;
        let digits = self.mantissa.unsigned_abs().to_string();
        let scale = usize::from(self.scale);
        let (int_part, frac_part) = if digits.len() > scale {
            digits.split_at(digits.len() - scale)
        } else {
            ("0", digits.as_str())
        };
        if negative && self.mantissa != 0 {
            write!(f, "-")?;
        }
        write!(f, "{int_part}")?;
        if scale > 0 {
            // Pad with leading zeros when the digit count is smaller than scale.
            if digits.len() < scale {
                write!(f, ".{:0>width$}", frac_part, width = scale)?;
            } else {
                write!(f, ".{frac_part}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Numeric;

    #[test]
    fn parse_and_display() {
        let cases = [
            ("0", "0"),
            ("42", "42"),
            ("-42", "-42"),
            ("3.14", "3.14"),
            ("0.10", "0.1"),
            ("1e3", "1000"),
            ("1.5e2", "150"),
            ("2.5e-2", "0.025"),
            ("007.5", "7.5"),
            ("-0.000", "0"),
        ];
        for (input, expected) in cases {
            let value = Numeric::parse(input).expect(input);
            assert_eq!(value.to_string(), expected, "input {input}");
        }
        assert!(Numeric::parse("abc").is_err());
        assert!(Numeric::parse("1.2.3").is_err());
        assert!(Numeric::parse("").is_err());
        assert!(Numeric::parse("1e").is_err());
    }

    #[test]
    fn arithmetic() {
        let a = Numeric::parse("1.10").unwrap();
        let b = Numeric::parse("2.05").unwrap();
        assert_eq!(a.clone().add(b.clone()).unwrap().to_string(), "3.15");
        assert_eq!(b.sub(a.clone()).unwrap().to_string(), "0.95");
        let c = Numeric::parse("1.5").unwrap();
        let d = Numeric::from_i64(2);
        assert_eq!(c.clone().mul(d.clone()).unwrap().to_string(), "3.0");
        assert_eq!(c.div(d).unwrap().to_string(), "0.7500");
        assert_eq!(
            Numeric::from_i64(7)
                .rem(Numeric::from_i64(3))
                .unwrap()
                .to_string(),
            "1"
        );
        assert!(Numeric::from_i64(1).div(Numeric::from_i64(0)).is_err());
    }

    #[test]
    fn ordering() {
        let a = Numeric::parse("-1.5").unwrap();
        let b = Numeric::from_i64(2);
        assert!(a < b);
        assert_eq!(
            a.cmp(&Numeric::parse("-1.50").unwrap()),
            std::cmp::Ordering::Equal
        );
        assert!(Numeric::from_i64(10) > Numeric::parse("9.99").unwrap());
    }

    #[test]
    fn conversions() {
        assert_eq!(Numeric::from_i64(-5).to_i64(), Some(-5));
        assert_eq!(Numeric::parse("1.5").unwrap().to_i64(), None);
        assert_eq!(Numeric::from_f64(1.25).to_f64(), 1.25);
    }
}
