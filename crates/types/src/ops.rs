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
//! PostgreSQL-compatible operator registry and evaluation hooks.

use crate::value::PgValue;
use plomid_core::{ErrorKind, PlomidError};
use std::collections::HashMap;
use std::sync::OnceLock;

type TResult<T> = std::result::Result<T, PlomidError>;

fn op_error(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message.into())
}

fn invalid_op(op: &str, detail: &str) -> PlomidError {
    op_error(format!("invalid operator {op}: {detail}"))
}

/// Binary operator discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `=` equality (is not distinct from).
    Equal,
    /// `<>` not equal.
    NotEqual,
    /// `<` less than.
    Less,
    /// `<=` less than or equal.
    LessOrEqual,
    /// `>` greater than.
    Greater,
    /// `>=` greater than or equal.
    GreaterOrEqual,
    /// `+` addition.
    Add,
    /// `-` subtraction.
    Subtract,
    /// `*` multiplication.
    Multiply,
    /// `/` division.
    Divide,
    /// `%` modulo.
    Modulo,
    /// `||` concatenation.
    Concat,
}

impl BinaryOp {
    /// All binary operators for registry iteration.
    #[must_use]
    pub const fn all() -> &'static [BinaryOp] {
        &[
            BinaryOp::Equal,
            BinaryOp::NotEqual,
            BinaryOp::Less,
            BinaryOp::LessOrEqual,
            BinaryOp::Greater,
            BinaryOp::GreaterOrEqual,
            BinaryOp::Add,
            BinaryOp::Subtract,
            BinaryOp::Multiply,
            BinaryOp::Divide,
            BinaryOp::Modulo,
            BinaryOp::Concat,
        ]
    }
}

/// Unary (prefix) operator discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Arithmetic negation `-x`.
    Negate,
    /// Arithmetic identity `+x`.
    Positive,
    /// Logical `NOT x`.
    Not,
}

impl UnaryOp {
    /// All unary operators.
    #[must_use]
    pub const fn all() -> &'static [UnaryOp] {
        &[UnaryOp::Negate, UnaryOp::Positive, UnaryOp::Not]
    }
}

fn op_equal(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(l.compare(r) == std::cmp::Ordering::Equal))
}

fn op_not_equal(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(l.compare(r) != std::cmp::Ordering::Equal))
}

fn op_less(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(l.compare(r) == std::cmp::Ordering::Less))
}

fn op_less_or_equal(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(
        l.compare(r) == std::cmp::Ordering::Less || l.compare(r) == std::cmp::Ordering::Equal,
    ))
}

fn op_greater(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(l.compare(r) == std::cmp::Ordering::Greater))
}

fn op_greater_or_equal(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    Ok(PgValue::Bool(
        l.compare(r) == std::cmp::Ordering::Greater || l.compare(r) == std::cmp::Ordering::Equal,
    ))
}

fn as_f64(v: &PgValue) -> Option<f64> {
    match v {
        PgValue::Int2(x) => Some(f64::from(*x)),
        PgValue::Int4(x) => Some(f64::from(*x)),
        PgValue::Int8(x) => Some(*x as f64),
        PgValue::Float4(x) => Some(f64::from(*x)),
        PgValue::Float8(x) => Some(*x),
        PgValue::Numeric(n) => Some(n.clone().to_f64()),
        PgValue::Money(x) => Some(*x as f64 / 100.0),
        _ => None,
    }
}

fn numeric_op<F: Fn(f64, f64) -> f64>(l: &PgValue, r: &PgValue, f: F) -> Option<PgValue> {
    let a = as_f64(l)?;
    let b = as_f64(r)?;
    Some(PgValue::Float8(f(a, b)))
}

fn op_add(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    let res = match (l, r) {
        (Int2(a), Int2(b)) => Int2(
            i16::try_from(i32::from(*a) + i32::from(*b))
                .map_err(|_| invalid_op("+", "smallint out of range"))?,
        ),
        (Int4(a), Int4(b)) => Int4(
            a.checked_add(*b)
                .ok_or_else(|| invalid_op("+", "integer out of range"))?,
        ),
        (Int8(a), Int8(b)) => Int8(
            a.checked_add(*b)
                .ok_or_else(|| invalid_op("+", "bigint out of range"))?,
        ),
        (Numeric(a), Numeric(b)) => {
            Numeric(a.clone().add(b.clone()).map_err(|e| invalid_op("+", &e))?)
        }
        _ => numeric_op(l, r, |a, b| a + b)
            .ok_or_else(|| invalid_op("+", "operands must be numeric"))?,
    };
    Ok(res)
}

fn op_subtract(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    let res = match (l, r) {
        (Int2(a), Int2(b)) => Int2(
            i16::try_from(i32::from(*a) - i32::from(*b))
                .map_err(|_| invalid_op("-", "smallint out of range"))?,
        ),
        (Int4(a), Int4(b)) => Int4(
            a.checked_sub(*b)
                .ok_or_else(|| invalid_op("-", "integer out of range"))?,
        ),
        (Int8(a), Int8(b)) => Int8(
            a.checked_sub(*b)
                .ok_or_else(|| invalid_op("-", "bigint out of range"))?,
        ),
        (Numeric(a), Numeric(b)) => {
            Numeric(a.clone().sub(b.clone()).map_err(|e| invalid_op("-", &e))?)
        }
        _ => numeric_op(l, r, |a, b| a - b)
            .ok_or_else(|| invalid_op("-", "operands must be numeric"))?,
    };
    Ok(res)
}

fn op_multiply(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    let res = match (l, r) {
        (Int2(a), Int2(b)) => Int2(
            i16::try_from(i32::from(*a) * i32::from(*b))
                .map_err(|_| invalid_op("*", "smallint out of range"))?,
        ),
        (Int4(a), Int4(b)) => Int4(
            a.checked_mul(*b)
                .ok_or_else(|| invalid_op("*", "integer out of range"))?,
        ),
        (Int8(a), Int8(b)) => Int8(
            a.checked_mul(*b)
                .ok_or_else(|| invalid_op("*", "bigint out of range"))?,
        ),
        (Numeric(a), Numeric(b)) => {
            Numeric(a.clone().mul(b.clone()).map_err(|e| invalid_op("*", &e))?)
        }
        _ => numeric_op(l, r, |a, b| a * b)
            .ok_or_else(|| invalid_op("*", "operands must be numeric"))?,
    };
    Ok(res)
}

fn op_divide(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    let res = match (l, r) {
        (Int2(a), Int2(b)) => {
            if *b == 0 {
                return Err(invalid_op("/", "division by zero"));
            }
            Int2(a / b)
        }
        (Int4(a), Int4(b)) => {
            if *b == 0 {
                return Err(invalid_op("/", "division by zero"));
            }
            Int4(a / b)
        }
        (Int8(a), Int8(b)) => {
            if *b == 0 {
                return Err(invalid_op("/", "division by zero"));
            }
            Int8(a / b)
        }
        (Numeric(a), Numeric(b)) => {
            let zero = crate::Numeric::from_i64(0);
            if *b == zero {
                return Err(invalid_op("/", "division by zero"));
            }
            Numeric(a.clone().div(b.clone()).map_err(|e| invalid_op("/", &e))?)
        }
        _ => {
            let a = as_f64(l).ok_or_else(|| invalid_op("/", "operands must be numeric"))?;
            let b = as_f64(r).ok_or_else(|| invalid_op("/", "operands must be numeric"))?;
            if b == 0.0 {
                return Err(invalid_op("/", "division by zero"));
            }
            PgValue::Float8(a / b)
        }
    };
    Ok(res)
}

fn op_modulo(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    let res = match (l, r) {
        (Int2(a), Int2(b)) => {
            if *b == 0 {
                return Err(invalid_op("%", "division by zero"));
            }
            Int2(a % b)
        }
        (Int4(a), Int4(b)) => {
            if *b == 0 {
                return Err(invalid_op("%", "division by zero"));
            }
            Int4(a % b)
        }
        (Int8(a), Int8(b)) => {
            if *b == 0 {
                return Err(invalid_op("%", "division by zero"));
            }
            Int8(a % b)
        }
        _ => {
            let a = as_f64(l).ok_or_else(|| invalid_op("%", "operands must be numeric"))?;
            let b = as_f64(r).ok_or_else(|| invalid_op("%", "operands must be numeric"))?;
            if b == 0.0 {
                return Err(invalid_op("%", "division by zero"));
            }
            PgValue::Float8(a % b)
        }
    };
    Ok(res)
}

fn op_concat(l: &PgValue, r: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    if l.is_null() || r.is_null() {
        return Ok(PgValue::Null);
    }
    fn string_value(value: &PgValue) -> Option<String> {
        match value {
            BpChar(s) | VarChar(s) | Text(s) | Name(s) | Xml(s) | Cstring(s) | Unknown(s) => {
                Some(s.clone())
            }
            _ => None,
        }
    }
    let lv = string_value(l).unwrap_or_else(|| l.to_sql_text());
    let rv = string_value(r).unwrap_or_else(|| r.to_sql_text());
    Ok(Text(format!("{lv}{rv}")))
}

type BinaryImpl = fn(&PgValue, &PgValue) -> TResult<PgValue>;
type UnaryImpl = fn(&PgValue) -> TResult<PgValue>;

/// Registered binary operator metadata.
#[derive(Clone, Debug)]
pub struct BinaryOpEntry {
    pub op: BinaryOp,
    pub implementation: BinaryImpl,
}

/// Registered unary operator metadata.
#[derive(Clone, Debug)]
pub struct UnaryOpEntry {
    pub op: UnaryOp,
    pub implementation: UnaryImpl,
}

/// Registry of known operators with their implementations.
#[derive(Debug, Default)]
pub struct OperatorRegistry {
    binary: HashMap<BinaryOp, BinaryOpEntry>,
    unary: HashMap<UnaryOp, UnaryOpEntry>,
}

impl OperatorRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a binary operator.
    pub fn register_binary(&mut self, entry: BinaryOpEntry) {
        self.binary.insert(entry.op, entry);
    }

    /// Registers a unary operator.
    pub fn register_unary(&mut self, entry: UnaryOpEntry) {
        self.unary.insert(entry.op, entry);
    }

    /// Looks up a binary operator.
    #[must_use]
    pub fn lookup_binary(&self, op: BinaryOp) -> Option<&BinaryOpEntry> {
        self.binary.get(&op)
    }

    /// Looks up a unary operator.
    #[must_use]
    pub fn lookup_unary(&self, op: UnaryOp) -> Option<&UnaryOpEntry> {
        self.unary.get(&op)
    }

    /// Applies a binary operator.
    pub fn apply_binary(&self, op: BinaryOp, left: PgValue, right: PgValue) -> TResult<PgValue> {
        let entry = self
            .lookup_binary(op)
            .ok_or_else(|| op_error(format!("operator {op:?} is not registered")))?;
        (entry.implementation)(&left, &right)
    }

    /// Applies a unary operator.
    pub fn apply_unary(&self, op: UnaryOp, operand: PgValue) -> TResult<PgValue> {
        let entry = self
            .lookup_unary(op)
            .ok_or_else(|| op_error(format!("unary operator {op:?} is not registered")))?;
        (entry.implementation)(&operand)
    }
}

fn op_unary_negate(v: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    Ok(match v {
        Null => Null,
        Int2(x) => Int2(
            x.checked_neg()
                .ok_or_else(|| invalid_op("-", "smallint out of range"))?,
        ),
        Int4(x) => Int4(
            x.checked_neg()
                .ok_or_else(|| invalid_op("-", "integer out of range"))?,
        ),
        Int8(x) => Int8(
            x.checked_neg()
                .ok_or_else(|| invalid_op("-", "bigint out of range"))?,
        ),
        Float4(x) => Float4(-*x),
        Float8(x) => Float8(-*x),
        Numeric(n) => Numeric(n.clone().neg()),
        Money(x) => Money(-*x),
        _ => return Err(invalid_op("-", "operand must be numeric")),
    })
}

fn op_unary_positive(v: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    Ok(match v {
        Null => Null,
        Int2(_) | Int4(_) | Int8(_) | Float4(_) | Float8(_) | Numeric(_) | Money(_) => v.clone(),
        _ => return Err(invalid_op("+", "operand must be numeric")),
    })
}

fn op_unary_not(v: &PgValue) -> TResult<PgValue> {
    use PgValue::*;
    Ok(match v {
        Null => Null,
        Bool(b) => Bool(!*b),
        _ => return Err(invalid_op("NOT", "operand must be boolean")),
    })
}

fn register_builtins() -> OperatorRegistry {
    let mut r = OperatorRegistry::new();
    use BinaryOp::*;
    for (op, imp) in [
        (Equal, op_equal as BinaryImpl),
        (NotEqual, op_not_equal as BinaryImpl),
        (Less, op_less as BinaryImpl),
        (LessOrEqual, op_less_or_equal as BinaryImpl),
        (Greater, op_greater as BinaryImpl),
        (GreaterOrEqual, op_greater_or_equal as BinaryImpl),
        (Add, op_add as BinaryImpl),
        (Subtract, op_subtract as BinaryImpl),
        (Multiply, op_multiply as BinaryImpl),
        (Divide, op_divide as BinaryImpl),
        (Modulo, op_modulo as BinaryImpl),
        (Concat, op_concat as BinaryImpl),
    ] {
        r.register_binary(BinaryOpEntry {
            op,
            implementation: imp,
        });
    }
    use UnaryOp::*;
    for (op, imp) in [
        (Negate, op_unary_negate as UnaryImpl),
        (Positive, op_unary_positive as UnaryImpl),
        (Not, op_unary_not as UnaryImpl),
    ] {
        r.register_unary(UnaryOpEntry {
            op,
            implementation: imp,
        });
    }
    r
}

/// Global built-in operator registry singleton.
#[must_use]
pub fn builtin_operators() -> &'static OperatorRegistry {
    static REG: OnceLock<OperatorRegistry> = OnceLock::new();
    REG.get_or_init(register_builtins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PgValue::*;

    #[test]
    fn binary_arith() {
        let reg = builtin_operators();
        let r = reg.apply_binary(BinaryOp::Add, Int4(2), Int4(3)).unwrap();
        assert_eq!(r, Int4(5));
        let r = reg
            .apply_binary(BinaryOp::Multiply, Int4(6), Int4(7))
            .unwrap();
        assert_eq!(r, Int4(42));
        let r = reg
            .apply_binary(BinaryOp::Divide, Float8(10.0), Float8(4.0))
            .unwrap();
        assert_eq!(r, Float8(2.5));
    }

    #[test]
    fn comparisons() {
        let reg = builtin_operators();
        assert_eq!(
            reg.apply_binary(BinaryOp::Less, Int4(1), Int4(2)).unwrap(),
            Bool(true)
        );
        assert_eq!(
            reg.apply_binary(BinaryOp::GreaterOrEqual, Int4(5), Int4(5))
                .unwrap(),
            Bool(true)
        );
        assert_eq!(
            reg.apply_binary(BinaryOp::NotEqual, Text("a".into()), Text("b".into()))
                .unwrap(),
            Bool(true)
        );
    }

    #[test]
    fn concat_op() {
        let reg = builtin_operators();
        let r = reg
            .apply_binary(BinaryOp::Concat, Text("a".into()), Text("b".into()))
            .unwrap();
        assert_eq!(r, Text("ab".into()));
        assert_eq!(
            reg.apply_binary(BinaryOp::Concat, Null, Text("x".into()))
                .unwrap(),
            Null
        );
    }

    #[test]
    fn div_by_zero_errors() {
        let reg = builtin_operators();
        assert!(reg
            .apply_binary(BinaryOp::Divide, Int4(1), Int4(0))
            .is_err());
        assert!(reg
            .apply_binary(BinaryOp::Modulo, Int8(1), Int8(0))
            .is_err());
    }

    #[test]
    fn unary_ops() {
        let reg = builtin_operators();
        assert_eq!(reg.apply_unary(UnaryOp::Negate, Int4(5)).unwrap(), Int4(-5));
        assert_eq!(
            reg.apply_unary(UnaryOp::Positive, Float8(3.5)).unwrap(),
            Float8(3.5)
        );
        assert_eq!(
            reg.apply_unary(UnaryOp::Not, Bool(true)).unwrap(),
            Bool(false)
        );
        assert_eq!(reg.apply_unary(UnaryOp::Not, Null).unwrap(), Null);
    }
}
