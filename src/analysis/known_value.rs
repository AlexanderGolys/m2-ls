//! Values the analysis knows at compile time, beyond their types: what lets
//! `S_6` evaluate to the generator `z` where its type says only that it is
//! an element of `S`, and `2^10` to `1024`.

use std::fmt::{self, Display, Formatter};

use super::ring::RingValue;

mod number;

pub use number::ExactNumber;

/// What is statically known of a ring: its generator count, characteristic and
/// Krull dimension, each absent when the program does not determine it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingProperties {
    /// The variables the ring adjoins to its coefficient ring: `numgens`.
    pub variables: usize,
    /// The characteristic, where it is determined.
    pub characteristic: Option<u128>,
    /// The Krull dimension, where it is determined.
    pub dimension: Option<u64>,
}

impl Display for RingProperties {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let noun = if self.variables == 1 {
            "variable"
        } else {
            "variables"
        };
        write!(formatter, "{} {noun}", self.variables)?;
        if let Some(characteristic) = self.characteristic {
            write!(formatter, ", char {characteristic}")?;
        }
        if let Some(dimension) = self.dimension {
            write!(formatter, ", dim {dimension}")?;
        }
        Ok(())
    }
}

/// The dimensions of a matrix the program writes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatrixShape {
    /// Number of rows: the generators of its target.
    pub rows: usize,
    /// Number of columns: the generators of its source.
    pub columns: usize,
}

impl Display for MatrixShape {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}×{} matrix", self.rows, self.columns)
    }
}

/// A value the program certainly computes, recorded on the binding state
/// holding it or derived from the expression supplying it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KnownValue {
    /// A ring the program writes out, `QQ[x, y]`.
    Ring(RingValue),
    /// What is known of a ring beyond which variables it has.
    RingProperties(RingProperties),
    /// An exact number.
    Number(ExactNumber),
    /// A truth value.
    Boolean(bool),
    /// A matrix whose dimensions the program determines.
    Matrix(MatrixShape),
    /// An ideal whose number of generators the program determines.
    Ideal {
        /// The generators written out.
        generators: usize,
    },
}

impl Display for KnownValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ring(_) => formatter.write_str("ring"),
            Self::RingProperties(properties) => properties.fmt(formatter),
            Self::Number(number) => number.fmt(formatter),
            Self::Boolean(value) => value.fmt(formatter),
            Self::Matrix(shape) => shape.fmt(formatter),
            Self::Ideal { generators } => {
                let noun = if *generators == 1 {
                    "generator"
                } else {
                    "generators"
                };
                write!(formatter, "ideal with {generators} {noun}")
            }
        }
    }
}
