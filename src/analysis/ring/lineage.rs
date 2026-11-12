//! The ring an expression lives in, where the program determines it.
//!
//! Macaulay2 rings are objects: every construction makes a new one, so
//! `QQ[x] === QQ[x]` and `A/I === A/I` are false, and elements of two
//! constructions never mix. What does mix is a ring with the rings in its
//! *lineage*, the chain of coefficient rings and quotiented rings it was built
//! from: `R = QQ[x]`, `T = R[y]`, `U = T/ideal y` give `R_0 + U_0` in `U`.
//! A number joins a ring when it promotes to the field the lineage ends in,
//! which is why `RR[x]` accepts `1/2` but `QQ[x]` rejects `1.5`. Anything else
//! is an error in the interpreter, which [`RingValues::ring_of`] answers
//! with `None`, like everything it cannot establish.
//!
//! A ring's identity here is the source it is constructed by, so a name that
//! is assigned twice, or a ring built in a loop, is never followed.

use std::fmt::{self, Display, Formatter};

use m2_syn::nodes::{Expr, ExprLiteral, ExprOp, ExprOpBin, ExprOpBinAdj};
use m2_syn::Token;
use strum::EnumString;

use super::values::RingValues;
use super::*;
use crate::analysis::InfixSyntax;
use crate::syntax::ExpressionSyntax;

/// How many names are followed to their definitions before resolution gives up.
const MAX_DEPTH: usize = 32;

/// The rings of numbers the interpreter implements natively, in the order
/// their elements promote: every integer is a rational, every real a complex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, EnumString)]
pub enum NumberField {
    /// The integers `ZZ`.
    #[strum(serialize = "ZZ")]
    Integers,
    /// The rationals `QQ`.
    #[strum(serialize = "QQ")]
    Rationals,
    /// The reals `RR`, of any precision.
    #[strum(serialize = "RR")]
    Reals,
    /// The complex numbers `CC`, of any precision.
    #[strum(serialize = "CC")]
    Complexes,
}

impl Display for NumberField {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Integers => "ZZ",
            Self::Rationals => "QQ",
            Self::Reals => "RR",
            Self::Complexes => "CC",
        })
    }
}

/// What a lineage ends in, which decides the numbers its rings accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Foundation {
    /// A native number ring: it accepts the numbers that promote to it.
    Numbers(NumberField),
    /// A finite field `ZZ/p` or `GF q`, which accepts integers and rationals.
    FiniteField,
}

impl Foundation {
    /// Whether a native number of `field` promotes into rings built on this.
    fn accepts(self, field: NumberField) -> bool {
        match self {
            Self::Numbers(base) => field <= base,
            Self::FiniteField => field <= NumberField::Rationals,
        }
    }
}

/// One ring object.
#[derive(Debug, Clone, Eq)]
pub struct RingIdentity {
    /// What distinguishes the ring from every other.
    key: RingKey,
    /// How the ring is written, for display; not part of its identity.
    label: String,
}

/// What makes two rings the same object.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RingKey {
    /// A native number ring, which exists once.
    Native(NumberField),
    /// A ring constructed by the expression at this source range.
    Constructed(SourceRangeKey),
}

impl PartialEq for RingIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Display for RingIdentity {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

/// A ring together with the rings it is built over, outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingLineage {
    /// The ring itself, then the rings of its coefficients or quotiented ring.
    rings: Vec<RingIdentity>,
    /// What the lineage ends in.
    foundation: Foundation,
}

impl RingLineage {
    /// The native number ring `field`.
    fn native(field: NumberField) -> Self {
        Self {
            rings: vec![RingIdentity {
                key: RingKey::Native(field),
                label: field.to_string(),
            }],
            foundation: Foundation::Numbers(field),
        }
    }

    /// The ring `label` constructed at `source`, over `base`.
    fn over(base: Self, source: SourceRangeKey, label: String, finite: bool) -> Self {
        let foundation = if finite {
            Foundation::FiniteField
        } else {
            base.foundation
        };
        let mut rings = vec![RingIdentity {
            key: RingKey::Constructed(source),
            label,
        }];
        rings.extend(base.rings);
        Self { rings, foundation }
    }

    /// This lineage with its ring called `label`, as the program names it.
    fn named(mut self, label: &str) -> Self {
        self.rings[0].label = label.to_owned();
        self
    }

    /// The ring itself.
    pub fn ring(&self) -> &RingIdentity {
        &self.rings[0]
    }

    /// Whether this is one of the native number rings, whose elements the
    /// interpreter computes with itself, rather than a ring the program builds.
    pub fn is_native(&self) -> bool {
        self.number_field().is_some()
    }

    /// The native number ring this is, if it is one.
    fn number_field(&self) -> Option<NumberField> {
        match self.ring().key {
            RingKey::Native(field) => Some(field),
            RingKey::Constructed(_) => None,
        }
    }

    /// Whether `other`'s ring is this ring or one it is built over.
    fn contains(&self, other: &Self) -> bool {
        self.rings.contains(other.ring())
    }

    /// The lineage in which an element of each of the two lives once added,
    /// or `None` when the interpreter rejects mixing them.
    pub fn join(self, other: Self) -> Option<Self> {
        if other.contains(&self) {
            return Some(other);
        }
        if self.contains(&other) {
            return Some(self);
        }
        match (self.number_field(), other.number_field()) {
            (Some(left), Some(right)) => Some(Self::native(left.max(right))),
            (Some(field), None) => other.foundation.accepts(field).then_some(other),
            (None, Some(field)) => self.foundation.accepts(field).then_some(self),
            (None, None) => None,
        }
    }
}

impl Display for RingLineage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.ring().fmt(formatter)
    }
}

impl<'ast, S, K, F> RingValues<'ast, '_, S, K, F>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
    F: Fn(&'ast Expr, usize) -> InferredType,
{
    /// The ring the value of `expression` lives in, when the program
    /// determines it: the ring of a generator, of a number, or the join of
    /// the rings of the operands of an arithmetic expression.
    pub fn ring_of(&self, expression: &'ast Expr) -> Option<RingLineage> {
        self.element_ring(expression, 0)
    }

    /// The ring of an element expression, following names at most
    /// [`MAX_DEPTH`] deep.
    fn element_ring(&self, expression: &'ast Expr, depth: usize) -> Option<RingLineage> {
        if depth > MAX_DEPTH {
            return None;
        }
        let expression = expression.unparenthesized()?;
        match expression {
            Expr::Literal(ExprLiteral::Integer(_)) => {
                Some(RingLineage::native(NumberField::Integers))
            }
            Expr::Literal(ExprLiteral::Float(_)) => Some(RingLineage::native(NumberField::Reals)),
            Expr::Symbol(symbol) => {
                let binding = self.binding(symbol)?;
                if binding.state.is_ring_generator {
                    let ring = binding.state.inferred_type.as_ref()?.single()?;
                    return self.generator_ring(ring, symbol);
                }
                if binding.binding.states.len() != 1 {
                    return None;
                }
                let supplied = self.expression_at(binding.state.value_range?)?;
                self.element_ring(supplied, depth + 1)
            }
            Expr::Op(ExprOp::Prefix(prefix))
                if prefix.operator.spelling() == <Token![-]>::SPELLING =>
            {
                self.element_ring(&prefix.operand, depth + 1)
            }
            operation @ Expr::Op(ExprOp::Bin(_)) => {
                match self.selected(operation) {
                    Some(Ok(selected)) => return self.ring_lineage(selected.ring, depth + 1),
                    Some(Err(_)) => return None,
                    None => {}
                }
                let operands = operation.infix_operands()?;
                if operands.operator == <Token![_]>::SPELLING {
                    return self.generator_at(operands.left, operands.right, depth);
                }
                self.arithmetic(operands.operator, operands.left, operands.right, depth)
            }
            _ => None,
        }
    }

    /// `R_n`, the `n`th variable of the ring `R`, which lives in `R` whatever
    /// integer `n` is: an index that is out of range fails, but never moves
    /// the result to another ring.
    fn generator_at(
        &self,
        ring: &'ast Expr,
        index: &'ast Expr,
        depth: usize,
    ) -> Option<RingLineage> {
        let ring = self.ring_lineage(ring, depth + 1)?;
        let index = self.element_ring(index, depth + 1)?;
        (index.number_field() == Some(NumberField::Integers) && !ring.is_native()).then_some(ring)
    }

    /// The ring of a generator, from the name of the ring it was introduced
    /// with, when that name denotes one ring throughout the program.
    fn generator_ring(&self, ring: &ObjectName, generator: &Symbol) -> Option<RingLineage> {
        let position = self.position(generator)?;
        let binding = self.global_binding(ring.name(), position)?;
        if binding.binding.states.len() != 1 {
            return None;
        }
        let supplied = self.expression_at(binding.state.value_range?)?;
        Some(self.ring_lineage(supplied, 0)?.named(ring.name()))
    }

    /// The ring of `left operator right` for an arithmetic operator.
    fn arithmetic(
        &self,
        operator: &str,
        left: &'ast Expr,
        right: &'ast Expr,
        depth: usize,
    ) -> Option<RingLineage> {
        let base = self.element_ring(left, depth + 1)?;
        if operator == <Token![^]>::SPELLING {
            let exponent = right.unparenthesized()?;
            return match (base.number_field(), exponent) {
                (Some(NumberField::Integers), Expr::Literal(ExprLiteral::Integer(_)))
                | (Some(NumberField::Rationals | NumberField::Reals | NumberField::Complexes), _)
                | (None, _) => Some(base),
                (Some(NumberField::Integers), _) => None,
            };
        }
        let other = self.element_ring(right, depth + 1)?;
        if operator == <Token![/]>::SPELLING {
            return match (base.number_field(), other.number_field()) {
                (Some(NumberField::Integers), Some(NumberField::Integers)) => {
                    Some(RingLineage::native(NumberField::Rationals))
                }
                (Some(_), Some(_)) => base.join(other),
                _ => None,
            };
        }
        let arithmetic = [
            <Token![+]>::SPELLING,
            <Token![-]>::SPELLING,
            <Token![*]>::SPELLING,
        ];
        arithmetic
            .contains(&operator)
            .then(|| base.join(other))
            .flatten()
    }

    /// The lineage of the ring `expression` constructs or names.
    fn ring_lineage(&self, expression: &'ast Expr, depth: usize) -> Option<RingLineage> {
        if depth > MAX_DEPTH {
            return None;
        }
        let expression = expression.unparenthesized()?;
        match expression {
            Expr::Symbol(symbol) => match self.binding(symbol) {
                None => symbol.text().parse().ok().map(RingLineage::native),
                Some(binding)
                    if binding.binding.states.len() == 1 && !binding.state.is_ring_generator =>
                {
                    let supplied = self.expression_at(binding.state.value_range?)?;
                    let lineage = self.ring_lineage(supplied, depth + 1)?;
                    Some(lineage.named(symbol.text()))
                }
                Some(_) => None,
            },
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => {
                self.constructed_ring(expression, call, depth)
            }
            quotient @ Expr::Op(ExprOp::Bin(_)) => {
                let operands = quotient.infix_operands()?;
                if operands.operator == <Token![_]>::SPELLING {
                    return self.precision_field(operands.left);
                }
                if operands.operator != <Token![/]>::SPELLING {
                    return None;
                }
                let base = self.ring_lineage(operands.left, depth + 1)?;
                let finite = base.number_field() == Some(NumberField::Integers);
                self.constructed(base, quotient, finite)
            }
            _ => None,
        }
    }

    /// `RR_53` and `CC_100`: a native number ring of a given precision.
    fn precision_field(&self, field: &'ast Expr) -> Option<RingLineage> {
        let Expr::Symbol(symbol) = field.unparenthesized()? else {
            return None;
        };
        if self.binding(symbol).is_some() {
            return None;
        }
        let field: NumberField = symbol.text().parse().ok()?;
        (field >= NumberField::Reals).then(|| RingLineage::native(field))
    }

    /// `coefficients[variables]` and `GF q`.
    fn constructed_ring(
        &self,
        whole: &'ast Expr,
        call: &'ast ExprOpBinAdj,
        depth: usize,
    ) -> Option<RingLineage> {
        if self.constructor_variables(call).is_some() {
            let coefficients = self.ring_lineage(&call.left_operand, depth + 1)?;
            return self.constructed(coefficients, whole, false);
        }
        let Expr::Symbol(constructor) = call.left_operand.unparenthesized()? else {
            return None;
        };
        if constructor.text() != "GF" || self.binding(constructor).is_some() {
            return None;
        }
        self.constructed(RingLineage::native(NumberField::Integers), whole, true)
    }

    /// The ring `expression` constructs over the lineage `base`.
    fn constructed(
        &self,
        base: RingLineage,
        expression: &Expr,
        finite: bool,
    ) -> Option<RingLineage> {
        let range = self.range(expression)?;
        let label = self.text(expression)?.to_owned();
        Some(RingLineage::over(
            base,
            source_range_key(range),
            label,
            finite,
        ))
    }
}
