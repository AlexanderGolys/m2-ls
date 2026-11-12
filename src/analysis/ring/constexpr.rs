//! Compile-time evaluation of the algebraic expressions whose value the
//! program determines: exact arithmetic and logic, ring invariants, and the
//! dimensions of matrices and ideals.
//!
//! Only what is cheap and certain is evaluated. Exact numbers are native to
//! the interpreter (`ZZ` and `QQ` arithmetic cannot be overloaded), so they
//! fold without regard to installed methods. A name is followed to its value
//! only when the program assigns it exactly once, which makes the value
//! independent of control flow. Everything else, a polynomial ring modulo an
//! ideal for one, stays unknown rather than approximated.

use std::cmp::Ordering;

use m2_syn::nodes::{Expr, ExprCollection, ExprLiteral, ExprOp, ExprOpBin, ExprOpBinAdj, Symbol};
use m2_syn::Token;
use strum::EnumString;

use super::values::RingValues;
use super::*;
use crate::analysis::known_value::{ExactNumber, MatrixShape, RingProperties};
use crate::analysis::InfixSyntax;
use crate::syntax::ExpressionSyntax;

/// How many names are followed to their values before evaluation gives up,
/// which also stops a name defined in terms of itself.
const MAX_DEPTH: usize = 32;

/// The largest exponent `^` evaluates, keeping a power cheap.
const LARGEST_EXPONENT: u32 = 1 << 12;

/// The most trial divisions spent finding the characteristic of a field `GF q`.
const MAX_TRIAL_DIVISIONS: i128 = 1 << 22;

/// The spelling of the truth value `true`.
const TRUE: &str = "true";

/// The spelling of the truth value `false`.
const FALSE: &str = "false";

/// The rings the program uses without defining them, whose invariants are
/// known: `ZZ` has dimension 1, the field `QQ` dimension 0, both characteristic 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
enum BaseRing {
    /// The integers.
    #[strum(serialize = "ZZ")]
    Integers,
    /// The rationals.
    #[strum(serialize = "QQ")]
    Rationals,
}

impl BaseRing {
    /// The invariants of the ring.
    fn properties(self) -> RingProperties {
        RingProperties {
            variables: 0,
            characteristic: Some(0),
            dimension: Some(match self {
                Self::Integers => 1,
                Self::Rationals => 0,
            }),
        }
    }
}

/// The builtin functions evaluated at compile time, by the name they are
/// called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
enum ConstantFunction {
    /// `binomial(n, k)`.
    #[strum(serialize = "binomial")]
    Binomial,
    /// `gcd(a, b)`.
    #[strum(serialize = "gcd")]
    Gcd,
    /// `lcm(a, b)`.
    #[strum(serialize = "lcm")]
    Lcm,
    /// `abs x`.
    #[strum(serialize = "abs")]
    Abs,
    /// `max(a, b, ...)`.
    #[strum(serialize = "max")]
    Max,
    /// `min(a, b, ...)`.
    #[strum(serialize = "min")]
    Min,
    /// `numgens R` or `numgens I`.
    #[strum(serialize = "numgens")]
    NumGens,
    /// `dim R`.
    #[strum(serialize = "dim")]
    Dim,
    /// `char R`.
    #[strum(serialize = "char")]
    Char,
    /// `numRows M`.
    #[strum(serialize = "numRows")]
    NumRows,
    /// `numColumns M`.
    #[strum(serialize = "numColumns")]
    NumColumns,
    /// `matrix {{...}, ...}`.
    #[strum(serialize = "matrix")]
    Matrix,
    /// `ideal(f, g, ...)` and `ideal {f, g, ...}`.
    #[strum(serialize = "ideal")]
    Ideal,
    /// `transpose M`.
    #[strum(serialize = "transpose")]
    Transpose,
}

/// A number as a known value.
fn number(value: ExactNumber) -> Option<KnownValue> {
    Some(KnownValue::Number(value))
}

/// A count as a known value.
fn count(value: usize) -> Option<KnownValue> {
    number(ExactNumber::integer(i128::try_from(value).ok()?))
}

impl<'ast, 'query, S, K, F> RingValues<'ast, 'query, S, K, F>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
    F: Fn(&'ast Expr, usize) -> InferredType,
{
    /// The value `expression` certainly evaluates to, when the program
    /// determines it statically.
    pub fn constant(&self, expression: &'ast Expr) -> Option<KnownValue> {
        self.evaluate(expression, 0)
    }

    /// The value of `expression`, following names at most [`MAX_DEPTH`] deep.
    fn evaluate(&self, expression: &'ast Expr, depth: usize) -> Option<KnownValue> {
        if depth > MAX_DEPTH {
            return None;
        }
        let expression = expression.unparenthesized()?;
        match expression {
            literal @ Expr::Literal(ExprLiteral::Integer(_)) => {
                let value = self.text(literal)?.trim().parse().ok()?;
                number(ExactNumber::integer(value))
            }
            Expr::Symbol(symbol) => self.named(symbol, depth),
            Expr::Op(ExprOp::Prefix(prefix)) => {
                let operand = self.evaluate(&prefix.operand, depth + 1)?;
                self.prefix(prefix.operator.spelling(), operand)
            }
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => self.call(expression, call, depth),
            operation @ Expr::Op(ExprOp::Bin(_)) => {
                let operands = operation.infix_operands()?;
                self.infix(operands.operator, operands.left, operands.right, depth)
            }
            _ => None,
        }
    }

    /// The value of a name: a truth value, or what its single assignment
    /// supplies.
    fn named(&self, symbol: &'ast Symbol, depth: usize) -> Option<KnownValue> {
        let Some(binding) = self.binding(symbol) else {
            return match symbol.text() {
                TRUE => Some(KnownValue::Boolean(true)),
                FALSE => Some(KnownValue::Boolean(false)),
                _ => self.ring_properties_of_name(symbol).map(Into::into),
            };
        };
        if binding.binding.states.len() != 1 || binding.state.is_ring_generator {
            return None;
        }
        let supplied = self.expression_at(binding.state.value_range?)?;
        self.evaluate(supplied, depth + 1)
    }

    /// `operator operand` for a prefix operator.
    fn prefix(&self, operator: &str, operand: KnownValue) -> Option<KnownValue> {
        match (operator, operand) {
            (<Token![-]>::SPELLING, KnownValue::Number(value)) => number(value.checked_neg()?),
            (<Token![+]>::SPELLING, value @ KnownValue::Number(_)) => Some(value),
            (<Token![not]>::SPELLING, KnownValue::Boolean(value)) => {
                Some(KnownValue::Boolean(!value))
            }
            _ => None,
        }
    }

    /// `left operator right`, where `and` and `or` stop at a decided left side.
    fn infix(
        &self,
        operator: &str,
        left: &'ast Expr,
        right: &'ast Expr,
        depth: usize,
    ) -> Option<KnownValue> {
        let left = self.evaluate(left, depth + 1)?;
        match (operator, &left) {
            (<Token![and]>::SPELLING, KnownValue::Boolean(false))
            | (<Token![or]>::SPELLING, KnownValue::Boolean(true)) => return Some(left),
            _ => {}
        }
        let right = self.evaluate(right, depth + 1)?;
        match (left, right) {
            (KnownValue::Number(left), KnownValue::Number(right)) => {
                Self::numbers(operator, left, right)
            }
            (KnownValue::Boolean(left), KnownValue::Boolean(right)) => {
                Self::booleans(operator, left, right)
            }
            (KnownValue::Matrix(left), KnownValue::Matrix(right)) => {
                Self::matrices(operator, left, right)
            }
            _ => None,
        }
    }

    /// An operator applied to two exact numbers.
    fn numbers(operator: &str, left: ExactNumber, right: ExactNumber) -> Option<KnownValue> {
        let ordering = || left.compare(right);
        let truth = |holds: fn(Ordering) -> bool| Some(KnownValue::Boolean(holds(ordering()?)));
        match operator {
            <Token![+]>::SPELLING => number(left.checked_add(right)?),
            <Token![-]>::SPELLING => number(left.checked_sub(right)?),
            <Token![*]>::SPELLING => number(left.checked_mul(right)?),
            <Token![/]>::SPELLING => number(left.checked_div(right)?),
            <Token![/ /]>::SPELLING => number(left.euclidean_quotient(right)?),
            <Token![%]>::SPELLING => number(left.euclidean_remainder(right)?),
            <Token![^]>::SPELLING => {
                number(left.checked_pow(right.as_integer()?, LARGEST_EXPONENT)?)
            }
            <Token![<]>::SPELLING => truth(Ordering::is_lt),
            <Token![>]>::SPELLING => truth(Ordering::is_gt),
            <Token![<=]>::SPELLING => truth(Ordering::is_le),
            <Token![>=]>::SPELLING => truth(Ordering::is_ge),
            <Token![==]>::SPELLING => truth(Ordering::is_eq),
            <Token![!=]>::SPELLING => truth(Ordering::is_ne),
            <Token![===]>::SPELLING => Some(KnownValue::Boolean(left.is_identical(right))),
            <Token![=!=]>::SPELLING => Some(KnownValue::Boolean(!left.is_identical(right))),
            _ => None,
        }
    }

    /// An operator applied to two truth values; the strict comparisons.
    fn booleans(operator: &str, left: bool, right: bool) -> Option<KnownValue> {
        let value = match operator {
            <Token![and]>::SPELLING => left && right,
            <Token![or]>::SPELLING => left || right,
            <Token![===]>::SPELLING => left == right,
            <Token![=!=]>::SPELLING => left != right,
            _ => return None,
        };
        Some(KnownValue::Boolean(value))
    }

    /// Sums and products of matrices whose dimensions agree.
    fn matrices(operator: &str, left: MatrixShape, right: MatrixShape) -> Option<KnownValue> {
        let shape = match operator {
            <Token![+]>::SPELLING | <Token![-]>::SPELLING => (left == right).then_some(left)?,
            <Token![*]>::SPELLING => (left.columns == right.rows).then_some(MatrixShape {
                rows: left.rows,
                columns: right.columns,
            })?,
            _ => return None,
        };
        Some(KnownValue::Matrix(shape))
    }

    /// A call of a builtin whose name the program does not redefine.
    fn call(
        &self,
        whole: &'ast Expr,
        call: &'ast ExprOpBinAdj,
        depth: usize,
    ) -> Option<KnownValue> {
        if let Some(properties) = self.ring_properties_of_call(whole, call, depth) {
            return Some(properties.into());
        }
        let Expr::Symbol(name) = call.left_operand.unparenthesized()? else {
            return None;
        };
        if self.binding(name).is_some() {
            return None;
        }
        let function = name.text().parse::<ConstantFunction>().ok()?;
        let arguments = call
            .right_operand
            .arguments()
            .into_iter()
            .collect::<Option<Vec<_>>>()?;
        self.apply(function, &arguments, depth)
    }

    /// The exact numbers an argument list evaluates to.
    fn number_arguments(&self, arguments: &[&'ast Expr], depth: usize) -> Option<Vec<ExactNumber>> {
        arguments
            .iter()
            .map(|argument| match self.evaluate(argument, depth + 1)? {
                KnownValue::Number(value) => Some(value),
                _ => None,
            })
            .collect()
    }

    /// `function` applied to `arguments`.
    fn apply(
        &self,
        function: ConstantFunction,
        arguments: &[&'ast Expr],
        depth: usize,
    ) -> Option<KnownValue> {
        use ConstantFunction::{
            Abs, Binomial, Char, Dim, Gcd, Ideal, Lcm, Matrix, Max, Min, NumColumns, NumGens,
            NumRows, Transpose,
        };
        match (function, arguments) {
            (Binomial | Gcd | Lcm, [_, _]) => {
                let [first, second] = self.number_arguments(arguments, depth)?[..] else {
                    return None;
                };
                number(match function {
                    Binomial => first.binomial(second)?,
                    Gcd => first.gcd(second)?,
                    _ => first.lcm(second)?,
                })
            }
            (Abs, [_]) => number(self.number_arguments(arguments, depth)?[0].checked_abs()?),
            (Max | Min, [_, ..]) => {
                let wanted = if function == Max {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
                let mut numbers = self.number_arguments(arguments, depth)?.into_iter();
                let mut best = numbers.next()?;
                for candidate in numbers {
                    if candidate.compare(best)? == wanted {
                        best = candidate;
                    }
                }
                number(best)
            }
            (NumGens, [argument]) => match self.evaluate(argument, depth + 1) {
                Some(KnownValue::Ideal { generators }) => count(generators),
                Some(KnownValue::RingProperties(properties)) => count(properties.variables),
                _ => count(self.ring_properties(argument, depth + 1)?.variables),
            },
            (Dim, [argument]) => {
                count(usize::try_from(self.ring_properties(argument, depth + 1)?.dimension?).ok()?)
            }
            (Char, [argument]) => number(ExactNumber::integer(
                i128::try_from(self.ring_properties(argument, depth + 1)?.characteristic?).ok()?,
            )),
            (NumRows | NumColumns | Transpose, [argument]) => {
                let KnownValue::Matrix(shape) = self.evaluate(argument, depth + 1)? else {
                    return None;
                };
                match function {
                    NumRows => count(shape.rows),
                    NumColumns => count(shape.columns),
                    _ => Some(KnownValue::Matrix(MatrixShape {
                        rows: shape.columns,
                        columns: shape.rows,
                    })),
                }
            }
            (Matrix, [.., rows]) => self.matrix_literal(rows),
            (Ideal, [generators]) => self
                .list_length(generators)
                .map(|generators| KnownValue::Ideal { generators }),
            (Ideal, [_, _, ..]) => Some(KnownValue::Ideal {
                generators: arguments.len(),
            }),
            _ => None,
        }
    }

    /// The number of elements of a list written out in the source.
    fn list_length(&self, list: &Expr) -> Option<usize> {
        let list = list.unparenthesized()?;
        matches!(list, Expr::Collection(ExprCollection::List(_)))
            .then(|| list.collection_elements().map(|elements| elements.len()))?
    }

    /// The dimensions of the matrix `{{...}, ...}` of rows written out, whose
    /// rows must have one length.
    fn matrix_literal(&self, rows: &Expr) -> Option<KnownValue> {
        let lengths = rows
            .unparenthesized()?
            .collection_elements()?
            .into_iter()
            .map(|row| self.list_length(row?))
            .collect::<Option<Vec<_>>>()?;
        let columns = *lengths.first()?;
        lengths
            .iter()
            .all(|length| *length == columns)
            .then_some(KnownValue::Matrix(MatrixShape {
                rows: lengths.len(),
                columns,
            }))
    }

    /// The invariants of a ring a call constructs: `GF q`, which is the field
    /// of `q` elements.
    fn ring_properties_of_call(
        &self,
        whole: &'ast Expr,
        call: &'ast ExprOpBinAdj,
        depth: usize,
    ) -> Option<RingProperties> {
        if let Some(properties) = self.polynomial_ring_properties(whole, call, depth) {
            return Some(properties);
        }
        let Expr::Symbol(constructor) = call.left_operand.unparenthesized()? else {
            return None;
        };
        if constructor.text() != GALOIS_FIELD || self.binding(constructor).is_some() {
            return None;
        }
        let KnownValue::Number(order) = self.evaluate(&call.right_operand, depth + 1)? else {
            return None;
        };
        Some(RingProperties {
            variables: 0,
            characteristic: Some(Self::prime_of_prime_power(order.as_integer()?)?),
            dimension: Some(0),
        })
    }

    /// The invariants of `coefficients[variables]`: a polynomial ring over a
    /// ring of known dimension has that dimension plus its variables.
    fn polynomial_ring_properties(
        &self,
        whole: &'ast Expr,
        constructor: &'ast ExprOpBinAdj,
        depth: usize,
    ) -> Option<RingProperties> {
        let Some(RingValue::Polynomial { variables, .. }) = self.value(whole) else {
            return None;
        };
        let adjoined = variables
            .iter()
            .map(|generator| generator.variable_count())
            .sum::<Option<usize>>()?;
        let coefficients = self.ring_properties(&constructor.left_operand, depth + 1)?;
        Some(RingProperties {
            variables: adjoined,
            characteristic: coefficients.characteristic,
            dimension: coefficients
                .dimension
                .and_then(|dimension| dimension.checked_add(u64::try_from(adjoined).ok()?)),
        })
    }

    /// The invariants of the ring `expression` constructs or names.
    fn ring_properties(&self, expression: &'ast Expr, depth: usize) -> Option<RingProperties> {
        if depth > MAX_DEPTH {
            return None;
        }
        match expression.unparenthesized()? {
            Expr::Symbol(symbol) => match self.binding(symbol) {
                None => self.ring_properties_of_name(symbol),
                Some(binding)
                    if binding.binding.states.len() == 1 && !binding.state.is_ring_generator =>
                {
                    let supplied = self.expression_at(binding.state.value_range?)?;
                    self.ring_properties(supplied, depth + 1)
                }
                Some(_) => None,
            },
            whole @ Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => {
                self.ring_properties_of_call(whole, call, depth)
            }
            quotient @ Expr::Op(ExprOp::Bin(_)) => {
                let operands = quotient.infix_operands()?;
                if operands.operator != <Token![/]>::SPELLING {
                    return None;
                }
                let ring = self.ring_properties(operands.left, depth + 1)?;
                if ring.variables != 0 || ring.dimension != Some(1) {
                    return None;
                }
                let KnownValue::Number(modulus) = self.evaluate(operands.right, depth + 1)? else {
                    return None;
                };
                let modulus = u128::try_from(modulus.as_integer()?)
                    .ok()
                    .filter(|m| *m >= 2)?;
                Some(RingProperties {
                    variables: 0,
                    characteristic: Some(modulus),
                    dimension: Some(0),
                })
            }
            _ => None,
        }
    }

    /// The invariants of a ring the program names without defining.
    fn ring_properties_of_name(&self, symbol: &Symbol) -> Option<RingProperties> {
        symbol
            .text()
            .parse::<BaseRing>()
            .ok()
            .map(BaseRing::properties)
    }

    /// The prime `p` when `order` is a power of `p`, as the order of a field
    /// `GF order` must be.
    fn prime_of_prime_power(order: i128) -> Option<u128> {
        if order < 2 {
            return None;
        }
        let mut divisor: i128 = 2;
        while divisor <= MAX_TRIAL_DIVISIONS && divisor * divisor <= order {
            if order % divisor == 0 {
                let mut rest = order;
                while rest % divisor == 0 {
                    rest /= divisor;
                }
                return (rest == 1).then_some(u128::try_from(divisor).ok()?);
            }
            divisor += 1;
        }
        (divisor * divisor > order).then_some(u128::try_from(order).ok()?)
    }
}

impl From<RingProperties> for KnownValue {
    fn from(properties: RingProperties) -> Self {
        Self::RingProperties(properties)
    }
}

/// The constructor of a finite field of a given order.
const GALOIS_FIELD: &str = "GF";
