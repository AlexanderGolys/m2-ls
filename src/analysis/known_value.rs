//! Values the analysis knows at compile time, beyond their types: what lets
//! `S_6` evaluate to the generator `z` where its type says only that it is
//! an element of `S`.

use super::ring::RingValue;

/// A value the program certainly computes, recorded on the binding state
/// holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KnownValue {
    /// A ring the program writes out, `QQ[x, y]`.
    Ring(RingValue),
}
