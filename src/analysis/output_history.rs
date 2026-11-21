//! Macaulay2's output variables, `oo`, `ooo`, `oooo` and `oN`, as the REPL
//! binds them after each top-level input.
//!
//! `Thing#AfterEval` in `Core.m2` runs once per input, muted by `;` or not:
//! a non-null value is bound to `oN`, `N` the input's line number, and
//! `rotateOutputLines` then shifts the stack
//!
//! ```m2
//! if ooo =!= null then global oooo <- ooo;
//! if oo =!= null then global ooo <- oo;
//! if x =!= null then global oo <- x;
//! ```
//!
//! Neither `oo` nor `ooo` is ever null — they start as their own symbols — so
//! every input shifts `ooo` and `oooo`, and only a non-null one replaces `oo`.
//! An input that errors runs no `AfterEval`: the variables stay as they were.

use std::num::NonZeroUsize;

use crate::typesystem::InferredType;

/// `oo`, `ooo` and `oooo`, nearest output first: the depth of the stack
/// `rotateOutputLines` keeps.
const RELATIVE_NAMES: [&str; 3] = ["oo", "ooo", "oooo"];

/// The prefix of `oN` at interpreter depth one; `Core.m2` repeats it once per
/// nested interpreter, which a document never starts.
const ABSOLUTE_PREFIX: char = 'o';

/// A reference to the output of an earlier input: `oo`, `ooo`, or `o3`.
#[derive(Debug, Clone, Copy)]
pub enum OutputReference {
    /// The output stack entry this many places back: `oo` is 1, `oooo` is 3.
    Relative(NonZeroUsize),
    /// The output of the input with this line number.
    Absolute(NonZeroUsize),
    /// `o0`, which no input numbers, or a number too large to have one.
    MissingAbsolute,
}

impl OutputReference {
    /// The reference `name` spells, if it is one.
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(index) = RELATIVE_NAMES.iter().position(|relative| *relative == name) {
            return NonZeroUsize::new(index + 1).map(Self::Relative);
        }
        let number = name.strip_prefix(ABSOLUTE_PREFIX)?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(
            number
                .parse()
                .ok()
                .and_then(NonZeroUsize::new)
                .map_or(Self::MissingAbsolute, Self::Absolute),
        )
    }
}

/// What one input leaves for the output variables: its value with `null`
/// taken out, and whether it may be `null`, which binds nothing.
///
/// `x = 2` leaves a `ZZ`; `print x` leaves nothing and may be null;
/// `if b then 2` leaves a `ZZ` and may be null; `error "e"` leaves nothing
/// and is never null, as it diverges.
#[derive(Debug, Clone)]
pub struct CellOutput {
    /// Classes of the value when it is not null.
    value: InferredType,
    /// Whether the value may be `null`.
    may_be_null: bool,
}

impl CellOutput {
    /// The output of an input whose value has classes `value` outside
    /// `Nothing`, and is `null` when `may_be_null`.
    pub fn new(value: InferredType, may_be_null: bool) -> Self {
        Self { value, may_be_null }
    }

    /// Whether the input never finishes, so runs no `AfterEval`.
    fn diverges(&self) -> bool {
        !self.may_be_null && self.value == InferredType::diverges()
    }

    /// What `oN` holds afterwards, `unbound` being the `Symbol` it is
    /// while nothing was assigned to it.
    pub fn absolute(&self, unbound: &InferredType) -> InferredType {
        if self.may_be_null || self.diverges() {
            self.value.clone() | unbound.clone()
        } else {
            self.value.clone()
        }
    }
}

/// The classes of `oo`, `ooo` and `oooo` at one point of the document.
///
/// After `1`, `"a"` and `print 2` they hold `String`, `String` and `ZZ`: the
/// null output of `print` shifted `ooo` and `oooo` but left `oo`.
#[derive(Debug, Clone)]
pub struct OutputStack([InferredType; RELATIVE_NAMES.len()]);

impl OutputStack {
    /// The stack before any output, each variable holding its own `Symbol`.
    pub fn initial(unbound: InferredType) -> Self {
        Self(RELATIVE_NAMES.map(|_| unbound.clone()))
    }

    /// The stack once `output` was rotated in.
    pub fn after(&self, output: &CellOutput) -> Self {
        if output.diverges() {
            return self.clone();
        }
        let [nearest, middle, _] = self.0.clone();
        let newest = if output.may_be_null {
            output.value.clone() | nearest.clone()
        } else {
            output.value.clone()
        };
        Self([newest, nearest, middle])
    }

    /// The entry `distance` places back.
    pub fn get(&self, distance: NonZeroUsize) -> &InferredType {
        &self.0[distance.get() - 1]
    }
}

/// The outputs of the first inputs of a document, and the stack after each.
///
/// Grown one input at a time, so an input referring to `oo` is inferred
/// against the history of the inputs before it.
#[derive(Debug)]
pub struct OutputHistory {
    /// What a variable no output was assigned to holds: its own `Symbol`.
    unbound: InferredType,
    /// The output of each input, in order.
    outputs: Vec<CellOutput>,
    /// The stack before each input, and after the last: one more than
    /// `outputs`.
    stacks: Vec<OutputStack>,
}

impl OutputHistory {
    /// The history before the first input.
    pub fn new(unbound: InferredType) -> Self {
        Self {
            stacks: vec![OutputStack::initial(unbound.clone())],
            unbound,
            outputs: Vec::new(),
        }
    }

    /// How many inputs the history covers.
    pub fn inputs(&self) -> usize {
        self.outputs.len()
    }

    /// Records the output of the next input.
    pub fn push(&mut self, output: CellOutput) {
        let stack = self.stacks[self.outputs.len()].after(&output);
        self.stacks.push(stack);
        self.outputs.push(output);
    }

    /// What `reference` holds while input `input`, counted from zero, runs.
    /// Needs the outputs of every input before it.
    pub fn reference(&self, reference: OutputReference, input: usize) -> InferredType {
        match reference {
            OutputReference::Relative(distance) => self.stacks[input].get(distance).clone(),
            OutputReference::Absolute(number) if number.get() <= input => {
                self.outputs[number.get() - 1].absolute(&self.unbound)
            }
            OutputReference::Absolute(_) | OutputReference::MissingAbsolute => self.unbound.clone(),
        }
    }
}
