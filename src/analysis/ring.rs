//! Ring construction, generator rebinding, and ring-specific type rules.

use m2_syn::nodes::{Array, ExprNew as NewStatement, FloatLiteral, IntegerLiteral, Symbol};
use m2_syn::Token;
use std::fmt::{self, Display, Formatter};

use crate::node_metadata::visit_expression_nodes;

use super::known_value::KnownValue;
use super::typechecker::{OperatorTypeQuery, TypeChecker};
use super::*;

/// The option of a field constructor such as `GF` naming its generator.
const VARIABLE_OPTION: &str = "Variable";

/// The function returning the symbol a string names, `getSymbol "x"`.
const GET_SYMBOL_FUNCTION: &str = "getSymbol";

/// The class of a generator whose ring has no name yet, as `x` of a bare
/// `QQ[x]`: an element of an anonymous polynomial ring.
const ANONYMOUS_RING_ELEMENT: &str = "RingElement";

/// The class of `x` once an index of it is written, `x_2 = 4` or `QQ[x_2]`.
const INDEXED_VARIABLE_TABLE: &str = "IndexedVariableTable";

/// The class of an index nothing wrote, `x_5` of `x_2 = 4`.
const INDEXED_VARIABLE: &str = "IndexedVariable";

/// The class of `RR` and `CC`, whose subscript `RR_53` is the field of that
/// precision.
const INEXACT_FIELD_FAMILY: &str = "InexactFieldFamily";

/// The field class each inexact field family subscripts into, `RR_53` a
/// `RealField`. `RRi` and `CCi` subscript into classes private to `Core`,
/// which source cannot name, so they read as the [`INEXACT_FIELD`] above.
const PRECISION_FIELD_CLASSES: [(&str, &str); 2] = [("RR", "RealField"), ("CC", "ComplexField")];

/// The public class of every field an inexact field family subscripts into.
const INEXACT_FIELD: &str = "InexactField";

/// The ring classes whose subscripts select variables. A ring of no class
/// at or above them, as `RR_53` or `GF 8`, has none, so it adds none to a
/// polynomial ring over it either.
const VARIABLE_RING_CLASSES: [&str; 2] = ["PolynomialRing", "QuotientRing"];

/// Runtime binding shape produced for a ring generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingGeneratorKind {
    /// Generator bound directly to a source name.
    Direct,
    /// Generator family represented by an indexed variable table.
    IndexedTable,
}

/// A ring-generator name and source node extracted from constructor syntax.
#[derive(Debug, Clone)]
struct RingGeneratorBinding<'tree> {
    /// Name used to resolve and present this item.
    name: String,
    /// Semantic or presentation category of this item.
    kind: RingGeneratorKind,
    /// Syntax supplying this item's source and structure.
    node: SyntaxNode<'tree>,
    /// Known index selection, absent when no indexed selection was specified.
    indices: Option<IndexSelection>,
}

/// Compact reference retained for later ring-generator rebinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingGenerator {
    /// Name used to resolve and present this item.
    name: ObjectName,
    /// Semantic or presentation category of this item.
    kind: RingGeneratorKind,
    /// Known index selection, absent when no indexed selection was specified.
    indices: Option<IndexSelection>,
}

/// Source-ordered writes to an indexed variable table, `x` once `x_2 = 4` or
/// `QQ[x_0 .. x_3]` has run. Each index reads as the latest write to it, and
/// an index nothing wrote is a bare `IndexedVariable`. Ranges stay compact
/// even when their Cartesian product contains many generators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedVariableTypes {
    /// Source-ordered updates used to determine the value at an index.
    writes: Vec<IndexedWrite>,
}

/// One write to indices of a table: what they hold from then on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedWrite {
    /// Indices affected by this operation.
    selection: IndexSelection,
    /// Runtime class of the selected or assigned value.
    value_type: ObjectName,
    /// The construct responsible for this indexed update.
    writer: IndexedWriter,
}

/// What wrote an index of an indexed variable table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
enum IndexedWriter {
    /// A ring constructor naming it a generator, `QQ[x_2]`.
    RingConstructor,
    /// An assignment to it, `x_2 = 4`.
    Assignment,
}

impl IndexedVariableTypes {
    /// The write `index` certainly reads: the latest one covering it.
    fn write_at(&self, index: &IndexKey) -> Option<&IndexedWrite> {
        self.writes
            .iter()
            .rev()
            .find(|write| write.selection.contains(index))
    }

    /// The types `index` may read: what the write it certainly reads stored,
    /// a bare `IndexedVariable` without one, together with what every later
    /// write to an index known only at run time stored.
    fn read_type(&self, index: &IndexKey) -> InferredType {
        let mut possible = None;
        for write in self.writes.iter().rev() {
            let written = InferredType::exact_from_id(write.value_type.clone());
            if write.selection == IndexSelection::Unknown {
                possible = Some(possible.map_or(written.clone(), |types| types | written));
            } else if write.selection.contains(index) {
                return possible.map_or(written.clone(), |types| types | written);
            }
        }
        let unwritten = InferredType::exact(INDEXED_VARIABLE);
        possible.map_or(unwritten.clone(), |types| types | unwritten)
    }

    /// The types an index no literal spells may read, the ring `A` of
    /// `x_A`: no literal write covers it, so it reads a bare
    /// `IndexedVariable` or what a write to an index known only at run time
    /// stored.
    fn read_nonliteral_type(&self) -> InferredType {
        self.writes
            .iter()
            .filter(|write| write.selection == IndexSelection::Unknown)
            .fold(InferredType::exact(INDEXED_VARIABLE), |types, write| {
                types | InferredType::exact_from_id(write.value_type.clone())
            })
    }
}

/// A statically known selector, including compound indices and quoted symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexKey {
    /// An integer-valued selector or literal.
    Integer(i64),
    /// A string-valued selector or source literal.
    String(String),
    /// A list of component selectors.
    List(Vec<IndexKey>),
    /// A sequence of component selectors.
    Sequence(Vec<IndexKey>),
    /// A symbol evaluating to itself, `E` of `x_E`.
    Symbol(ObjectName),
    /// An indexed variable of such a symbol, `E_F` of `x_(E_F)`.
    IndexedVariable {
        /// Name on which the indexed selection operates.
        base: ObjectName,
        /// Index selecting an entry in the enclosing representation.
        index: Box<IndexKey>,
    },
}

/// Whether a symbol node evaluates to itself where an index spells it, so
/// the index is the key it spells: `E` of `x_E` when nothing binds `E`.
type BareSymbols<'resolver> = &'resolver dyn Fn(SyntaxNode<'_>) -> bool;

impl IndexKey {
    /// The key a literal index spells, with no symbol in it.
    fn literal(node: SyntaxNode<'_>) -> Option<Self> {
        Self::from_node(node, &|_| false)
    }

    /// The key the index `node` spells: a literal, or symbols that
    /// `bare_symbols` says evaluate to themselves and indexed variables of
    /// them.
    fn from_node(node: SyntaxNode<'_>, bare_symbols: BareSymbols<'_>) -> Option<Self> {
        let node = parenthesized_value(node)?;
        if node.is::<IntegerLiteral>() {
            return node.text().parse().ok().map(Self::Integer);
        }
        if node.is_prefix_expr() && node.operator_node()?.text() == "-" {
            let Self::Integer(value) = Self::from_node(node.unary_operand()?, bare_symbols)? else {
                return None;
            };
            return value.checked_neg().map(Self::Integer);
        }
        if node.is::<Symbol>() {
            return bare_symbols(node).then(|| Self::Symbol(ObjectName::new(node.text())));
        }
        if node.has_binary_operator::<Token![_]>() {
            let parts = node.binary_parts()?;
            let base = parenthesized_value(parts.left)?;
            return (base.is::<Symbol>() && bare_symbols(base))
                .then(|| Self::from_node(parts.right, bare_symbols))
                .flatten()
                .map(|index| Self::IndexedVariable {
                    base: ObjectName::new(base.text()),
                    index: Box::new(index),
                });
        }
        if let Some(text) = node.string_literal_inner_text() {
            // Escaped strings require evaluation; do not compare their raw spelling.
            return (!text.contains('\\')).then(|| Self::String(text.to_string()));
        }
        if node.is::<List>() || node.is::<Sequence>() {
            let items = node
                .collection_elements()
                .map(|element| Self::from_node(element, bare_symbols))
                .collect::<Option<Vec<_>>>()?;
            return Some(if node.is::<List>() {
                Self::List(items)
            } else {
                Self::Sequence(items)
            });
        }
        None
    }

    /// Tests whether this key falls within the specified endpoints and upper-bound policy.
    fn in_range(&self, start: &Self, end: &Self, exclusive: bool) -> bool {
        match (self, start, end) {
            (Self::Integer(value), Self::Integer(start), Self::Integer(end)) => {
                start <= value && if exclusive { value < end } else { value <= end }
            }
            (Self::List(values), Self::List(starts), Self::List(ends))
            | (Self::Sequence(values), Self::Sequence(starts), Self::Sequence(ends)) => {
                values.len() == starts.len()
                    && values.len() == ends.len()
                    && values
                        .iter()
                        .zip(starts)
                        .zip(ends)
                        .all(|((value, start), end)| value.in_range(start, end, exclusive))
            }
            (Self::String(value), Self::String(start), Self::String(end)) => {
                value.chars().count() == start.chars().count()
                    && value.chars().count() == end.chars().count()
                    && value.chars().zip(start.chars()).zip(end.chars()).all(
                        |((value, start), end)| {
                            start <= value && if exclusive { value < end } else { value <= end }
                        },
                    )
            }
            _ => false,
        }
    }

    /// How many keys the range from `self` to `end` spans: integers count up,
    /// and sequences and lists span the box of their components, so a
    /// descending component spans nothing. `None` for a range this analysis
    /// does not enumerate, as one of strings.
    fn span_len(&self, end: &Self, exclusive: bool) -> Option<usize> {
        match (self, end) {
            (Self::Integer(start), Self::Integer(end)) => {
                let len = end
                    .checked_sub(*start)?
                    .checked_add(i64::from(!exclusive))?;
                Some(usize::try_from(len).unwrap_or(0))
            }
            (Self::List(starts), Self::List(ends))
            | (Self::Sequence(starts), Self::Sequence(ends))
                if starts.len() == ends.len() =>
            {
                starts
                    .iter()
                    .zip(ends)
                    .try_fold(1_usize, |len, (start, end)| {
                        len.checked_mul(start.span_len(end, exclusive)?)
                    })
            }
            _ => None,
        }
    }

    /// The `n`th key of the range from `self` to `end`, in the order
    /// Macaulay2 expands it: the last component of a box varies fastest.
    fn span_nth(&self, end: &Self, exclusive: bool, n: usize) -> Option<Self> {
        if n >= self.span_len(end, exclusive)? {
            return None;
        }
        match (self, end) {
            (Self::Integer(start), Self::Integer(_)) => {
                start.checked_add(i64::try_from(n).ok()?).map(Self::Integer)
            }
            (Self::List(starts), Self::List(ends))
            | (Self::Sequence(starts), Self::Sequence(ends)) => {
                let mut rest = n;
                let mut components = starts
                    .iter()
                    .zip(ends)
                    .rev()
                    .map(|(start, end)| {
                        let len = start.span_len(end, exclusive)?;
                        let component = start.span_nth(end, exclusive, rest % len);
                        rest /= len;
                        component
                    })
                    .collect::<Option<Vec<_>>>()?;
                components.reverse();
                Some(if matches!(self, Self::List(_)) {
                    Self::List(components)
                } else {
                    Self::Sequence(components)
                })
            }
            _ => None,
        }
    }
}

/// The key as Macaulay2 prints it: `(1,1)`, `{1, 2}`, `E_F`, and a string
/// without quotes.
impl Display for IndexKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let join = |keys: &[Self], separator| {
            keys.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(separator)
        };
        match self {
            Self::Integer(value) => write!(f, "{value}"),
            Self::String(text) => f.write_str(text),
            Self::List(keys) => write!(f, "{{{}}}", join(keys, ", ")),
            Self::Sequence(keys) => write!(f, "({})", join(keys, ",")),
            Self::Symbol(name) => f.write_str(name.name()),
            Self::IndexedVariable { base, index } => {
                write!(f, "{}_{}", base.name(), index.subscript())
            }
        }
    }
}

impl IndexKey {
    /// The key as it follows `_` in a printed indexed variable, parenthesized
    /// unless atomic: `x_(-1)`, `x_(E_F)`.
    fn subscript(&self) -> String {
        match self {
            Self::Integer(value) if *value < 0 => format!("({self})"),
            Self::IndexedVariable { .. } => format!("({self})"),
            _ => self.to_string(),
        }
    }
}

/// An exact selector, a bounded range, or a selection unavailable statically.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexSelection {
    /// A single known identity, value, or type point.
    Exact(IndexKey),
    /// A bounded interval of selectors.
    Range {
        /// Inclusive beginning of the represented interval.
        start: IndexKey,
        /// Exclusive end of the represented interval.
        end: IndexKey,
        /// Whether the upper endpoint is excluded from the selection.
        exclusive: bool,
    },
    /// An index known only at run time, the `i` of `x_i = i` in a loop: it
    /// may be any index, so it never certainly contains one.
    Unknown,
}

impl IndexSelection {
    /// Tests membership in the represented selection or source extent.
    fn contains(&self, key: &IndexKey) -> bool {
        match self {
            Self::Exact(index) => index == key,
            Self::Range {
                start,
                end,
                exclusive,
            } => key.in_range(start, end, *exclusive),
            Self::Unknown => false,
        }
    }

    /// How many indices the selection names; `None` when that is not known.
    fn key_count(&self) -> Option<usize> {
        match self {
            Self::Exact(_) => Some(1),
            Self::Range {
                start,
                end,
                exclusive,
            } => start.span_len(end, *exclusive),
            Self::Unknown => None,
        }
    }

    /// The `n`th index the selection names, in Macaulay2's expansion order.
    fn key_at(&self, n: usize) -> Option<IndexKey> {
        match self {
            Self::Exact(index) => (n == 0).then(|| index.clone()),
            Self::Range {
                start,
                end,
                exclusive,
            } => start.span_nth(end, *exclusive, n),
            Self::Unknown => None,
        }
    }
}

/// One variable of a ring as Macaulay2 prints it: `z`, `x_0`, `w_(1,1)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingVariable {
    /// Name on which the indexed selection operates.
    base: ObjectName,
    /// Index selecting an entry in the enclosing representation.
    index: Option<IndexKey>,
}

impl Display for RingVariable {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.index {
            Some(index) => write!(f, "{}_{}", self.base.name(), index.subscript()),
            None => f.write_str(self.base.name()),
        }
    }
}

impl From<&RingGeneratorBinding<'_>> for RingGenerator {
    fn from(binding: &RingGeneratorBinding<'_>) -> Self {
        Self {
            name: ObjectName::new(&binding.name),
            kind: binding.kind,
            indices: binding.indices.clone(),
        }
    }
}

impl RingGenerator {
    /// How many variables the generator declares: one for a plain name, each
    /// index of its selection for an indexed family; `None` when unknown.
    fn variable_count(&self) -> Option<usize> {
        match self.kind {
            RingGeneratorKind::Direct => Some(1),
            RingGeneratorKind::IndexedTable => self.indices.as_ref()?.key_count(),
        }
    }

    /// The `n`th variable the generator declares.
    fn variable(&self, n: usize) -> Option<RingVariable> {
        let index = match self.kind {
            RingGeneratorKind::Direct => (n == 0).then_some(None)?,
            RingGeneratorKind::IndexedTable => Some(self.indices.as_ref()?.key_at(n)?),
        };
        Some(RingVariable {
            base: self.name.clone(),
            index,
        })
    }
}

/// A ring the program writes out, as far as its subscripts `R_i` reach:
/// which variables they select. Built from source alone, never from a type —
/// a `PolynomialRing` in general has variables no analysis can name, while
/// `QQ[x]` has exactly `x` over `QQ`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RingValue {
    /// A ring whose subscripts select no variables and which adds none to a
    /// ring over it: `QQ`, `ZZ`, `RR_53`, `GF 8`.
    Basic,
    /// `coefficient_ring[variables]`, the coefficient ring `None` when the
    /// program does not determine it statically.
    Polynomial {
        /// Known coefficient ring, absent when it cannot be established statically.
        coefficient_ring: Option<Box<RingValue>>,
        /// Ring generators in the order used by positional selection.
        variables: Vec<RingGenerator>,
    },
    /// `ring / ideal`, whose subscripts select the images of those of `ring`.
    Quotient(Box<RingValue>),
}

impl RingValue {
    /// The variables the subscripts select, in order: a polynomial ring's
    /// own, then down its tower those of its coefficient ring, `R_1` of
    /// `R = QQ[a][b]` being `a`. `None` when subscripts select no variables.
    fn variables(&self) -> Option<RingVariables<'_>> {
        match self {
            Self::Basic => None,
            Self::Polynomial {
                coefficient_ring,
                variables,
            } => {
                let mut tower = RingVariables {
                    generators: variables.iter().collect(),
                    complete: true,
                };
                match coefficient_ring.as_deref().map(Self::variables) {
                    Some(Some(coefficient_variables)) => {
                        tower.generators.extend(coefficient_variables.generators);
                        tower.complete = coefficient_variables.complete;
                    }
                    Some(None) => {}
                    None => tower.complete = false,
                }
                Some(tower)
            }
            Self::Quotient(ring) => ring.variables(),
        }
    }

    /// The variable `R_index` selects, or why Macaulay2 rejects the index;
    /// `None` when that is not known.
    pub fn subscript(&self, index: i64) -> Option<Result<RingVariable, RingIndexOutOfBounds>> {
        self.variables()?.get(index)
    }

    /// The variable named `name` among all those the subscripts select, as
    /// Macaulay2 indexes them by their printed names.
    fn search(&self, name: &VariableName) -> VariableSearch {
        let Some(variables) = self.variables() else {
            return VariableSearch::Absent;
        };
        for generator in variables.generators {
            let Some(count) = generator.variable_count() else {
                return VariableSearch::Undetermined;
            };
            if !name.0.starts_with(generator.name.name()) {
                continue;
            }
            if let Some(variable) = (0..count)
                .filter_map(|n| generator.variable(n))
                .find(|variable| variable.to_string() == name.0)
            {
                return VariableSearch::Found(variable);
            }
        }
        if variables.complete {
            VariableSearch::Absent
        } else {
            VariableSearch::Undetermined
        }
    }
}

/// A subscript `R_index` past the variables of `R`, which Macaulay2 rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingIndexOutOfBounds {
    /// Index selecting an entry in the enclosing representation.
    pub index: i64,
    /// Number of available ring generators.
    pub variable_count: usize,
}

/// Macaulay2's own message: `index 2 out of bounds 0 .. 1`.
impl Display for RingIndexOutOfBounds {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let last = i128::try_from(self.variable_count).map_or(i128::MAX, |count| count - 1);
        write!(f, "index {} out of bounds 0 .. {last}", self.index)
    }
}

/// A ring variable's name as Macaulay2 prints it, `x_1`: what `"x_1"_R`,
/// `x_1_R` and `S_0_R` look the variable up by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableName(String);

impl Display for VariableName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the left operand of `v_R` is, which picks the method Macaulay2
/// looks the variable `v` up in `R` with, and what it does without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VariableLookup {
    /// `String _ Ring`, `"x"_R`.
    String,
    /// `Symbol _ Ring`, `x_R` with `x` unbound — or bound by a package the
    /// analysis does not see, so a failed lookup is never claimed.
    Symbol,
    /// `IndexedVariable _ Ring`, `x_1_R`.
    IndexedVariable,
    /// `RingElement _ Ring`, `x_1_B_R`: a variable of another ring, looked
    /// up by its name or else promoted, which may succeed.
    RingElement,
}

impl VariableLookup {
    /// Macaulay2's error when `R` has no variable of the name, where that
    /// certainly ends the lookup.
    fn failure(self) -> Option<&'static str> {
        match self {
            Self::String => Some("variable not found in ring"),
            Self::IndexedVariable => Some("indexed variable not found in ring"),
            Self::Symbol | Self::RingElement => None,
        }
    }
}

/// Why Macaulay2 rejects an expression selecting a variable of a ring.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RingVariableError {
    /// A subscript `R_i` past the variables of `R`.
    IndexOutOfBounds(RingIndexOutOfBounds),
    /// A lookup `v_R` of a name no variable of `R` has.
    NotFound {
        /// Name used to resolve and present this item.
        name: VariableName,
        /// Explanation of why the variable selection cannot succeed.
        failure: &'static str,
    },
}

/// Macaulay2's own message, with the name a lookup missed.
impl Display for RingVariableError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IndexOutOfBounds(out_of_bounds) => out_of_bounds.fmt(f),
            Self::NotFound { name, failure } => write!(f, "{failure}: `{name}`"),
        }
    }
}

/// A variable of a ring the program writes out, as an expression selects
/// it: `S_6`, `"x"_A`, `x_1_A`. `ring` is the operand naming the ring.
#[derive(Debug, Clone)]
pub struct SelectedRingVariable<'tree> {
    /// Expression supplying the ring being selected from.
    pub ring: SyntaxNode<'tree>,
    /// Resolved ring variable selected by the source expression.
    pub variable: RingVariable,
}

/// Whether a ring has a variable of some name.
enum VariableSearch {
    /// A statically resolved ring variable.
    Found(RingVariable),
    /// No matching value or declaration is present.
    Absent,
    /// Available static evidence cannot determine the result.
    Undetermined,
}

/// The variables the subscripts of a ring select, in order. Where part of
/// its tower is not known, only a prefix is: `complete` tells whether
/// `generators` ends where the ring's variables do.
struct RingVariables<'ring> {
    /// Generators contributing to the represented set or type family.
    generators: Vec<&'ring RingGenerator>,
    /// Whether every contributing value is known statically.
    complete: bool,
}

impl RingVariables<'_> {
    /// How many variables the ring has; `None` unless all are known.
    fn count(&self) -> Option<usize> {
        self.complete.then_some(())?;
        self.generators
            .iter()
            .try_fold(0_usize, |count, generator| {
                count.checked_add(generator.variable_count()?)
            })
    }

    /// The variable `R_index` selects, a negative index counting from the
    /// end, or the bounds it breaks; `None` when that is not known.
    fn get(&self, index: i64) -> Option<Result<RingVariable, RingIndexOutOfBounds>> {
        let out_of_bounds = |variable_count| RingIndexOutOfBounds {
            index,
            variable_count,
        };
        let mut offset = if index < 0 {
            let count = self.count()?;
            match usize::try_from(index.unsigned_abs()) {
                Ok(distance) if distance <= count => count - distance,
                _ => return Some(Err(out_of_bounds(count))),
            }
        } else {
            usize::try_from(index).ok()?
        };
        for generator in &self.generators {
            let count = generator.variable_count()?;
            if offset < count {
                return generator.variable(offset).map(Ok);
            }
            offset -= count;
        }
        Some(Err(out_of_bounds(self.count()?)))
    }
}

/// Hardcoded static semantics that supplement indexed ring dispatch facts.
pub struct RingSemantics;

impl RingSemantics {
    /// Return the instance parent introduced by a source-defined ring value.
    pub fn value_parent(
        type_name: Option<&ObjectName>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        type_name
            .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Ring))
            .then(|| ObjectName::new("RingElement"))
    }

    /// Whether a value of type `inferred` may be a ring: one of its points is
    /// a ring, or one of its upper bounds is a ring or lies above `Ring`, as
    /// the `Thing` of an expression inference could not type.
    pub fn may_be_ring(inferred: &InferredType, knowledge: &(impl TypeKnowledge + ?Sized)) -> bool {
        let ring = TypeRole::Ring.object_name();
        inferred
            .exact_points()
            .any(|point| knowledge.has_type_role(point, TypeRole::Ring))
            || inferred.upward_generators().any(|generator| {
                knowledge.has_type_role(generator, TypeRole::Ring)
                    || knowledge.is_subtype(&ring, generator)
            })
    }
}

impl Analysis {
    /// Registers the statically recognizable generators introduced by a ring expression.
    pub fn collect_ring_generator_bindings(
        &mut self,
        ring_name: &str,
        expression: (SyntaxNode, Option<&Expr>),
        rebind_node: SyntaxNode,
        scope_idx: usize,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        let (expression, syntax) = expression;
        // Generators rebound to a quotient are registered at the ring's own
        // target range, so its state is resolved before they take that key.
        let ring_state = self
            .registry
            .binding_states_by_range
            .get(&source_range_key(source.range_for_node(rebind_node)))
            .copied();
        let mut nodes = Vec::new();
        visit_expression_nodes(expression, syntax, |node| nodes.push(node));
        let mut specs = nodes
            .into_iter()
            .filter_map(|node| self.ring_constructor_variables(node, source, knowledge, scope_idx))
            .map(|variables| {
                (
                    variables,
                    RingGeneratorBinding::collect(variables, &|symbol| {
                        self.is_bare_symbol(symbol, source, knowledge)
                    }),
                )
            })
            .collect::<Vec<_>>();
        specs.extend(RingGeneratorBinding::variable_option(
            expression,
            &|symbol| self.is_bare_symbol(symbol, source, knowledge),
        ));

        let mut generators = Vec::new();
        for (spec, bindings) in specs {
            self.registry
                .ring_generator_specs
                .push(source.range_for_node(spec));
            for generator in bindings {
                generators.push(RingGenerator::from(&generator));
                self.register_ring_generator(
                    ring_name,
                    &generator.name,
                    generator.kind,
                    generator.indices,
                    generator.node,
                    source,
                );
            }
        }

        if generators.is_empty() {
            generators = self
                .ring_source_symbol(expression)
                .and_then(|source| self.registry.ring_generators.get(source).cloned())
                .unwrap_or_default();
            for generator in &generators {
                self.register_ring_generator(
                    ring_name,
                    generator.name.name(),
                    generator.kind,
                    generator.indices.clone(),
                    rebind_node,
                    source,
                );
            }
        }

        self.registry
            .ring_generators
            .insert(ObjectName::new(ring_name), generators);

        let value = self.ring_value(expression, source, knowledge, scope_idx);
        if let Some(state) = ring_state.and_then(|id| {
            self.registry
                .bindings
                .get_mut(id.binding.0 as usize)?
                .states
                .get_mut(id.state)
        }) {
            state.known_value = value.map(KnownValue::Ring);
        }
    }

    /// The variable of a ring the program writes out that `expression`
    /// selects, or why Macaulay2 rejects it: a subscript `S_6` by a literal
    /// index, `z` after `S = RR_7[x_0..y_2, z, w_(1,1)]`, or a lookup by
    /// name `"x"_A`, `x_1_A`, `S_0_A`. `None` when the program does not
    /// determine either.
    pub fn selected_ring_variable<'tree>(
        &self,
        expression: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Result<SelectedRingVariable<'tree>, RingVariableError>> {
        let expression = parenthesized_value(expression)?;
        let parts = expression
            .binary_parts()
            .filter(|_| expression.has_binary_operator::<Token![_]>())?;
        self.subscript_selection(parts.left, parts.right, source, knowledge)
    }

    /// What `left _ right` selects: a variable of the written ring `left` by
    /// the literal index `right`, or of the written ring `right` by the name
    /// `left` gives.
    pub(super) fn subscript_selection<'tree>(
        &self,
        left: SyntaxNode<'tree>,
        right: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Result<SelectedRingVariable<'tree>, RingVariableError>> {
        if let Some(ring) = self.written_ring(left, source) {
            let IndexKey::Integer(index) = IndexKey::literal(right)? else {
                return None;
            };
            return Some(
                ring.subscript(index)?
                    .map(|variable| SelectedRingVariable {
                        ring: left,
                        variable,
                    })
                    .map_err(RingVariableError::IndexOutOfBounds),
            );
        }
        let ring = self.written_ring(right, source)?;
        let (name, lookup) = self.variable_name(left, source, knowledge)?;
        match ring.search(&name) {
            VariableSearch::Found(variable) => Some(Ok(SelectedRingVariable {
                ring: right,
                variable,
            })),
            VariableSearch::Absent => lookup
                .failure()
                .map(|failure| Err(RingVariableError::NotFound { name, failure })),
            VariableSearch::Undetermined => None,
        }
    }

    /// Whether `left _ right` reads an unindexed ring variable `left` at
    /// the index `right`, which Macaulay2 answers with the indexed variable
    /// of its name: `x_1` for `A_0_1` when `A_0` is `x`, or for `x_1` when
    /// `x` holds a ring generator.
    pub(super) fn indexes_ring_variable(
        &self,
        left: SyntaxNode<'_>,
        right: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let bare_symbols = |symbol: SyntaxNode<'_>| self.is_bare_symbol(symbol, source, knowledge);
        IndexKey::from_node(right, &bare_symbols).is_some()
            && match parenthesized_value(left) {
                Some(symbol) if symbol.is::<Symbol>() => {
                    let position = source.position_for_node(symbol);
                    let scope_idx = self.find_scope_at(position).unwrap_or(0);
                    self.get_binding_from_scope(symbol.text(), scope_idx, position)
                        .is_some_and(|binding| {
                            binding.state.is_ring_generator
                                && binding.state.indexed_element_type.is_none()
                        })
                }
                _ => matches!(
                    self.selected_ring_variable(left, source, knowledge),
                    Some(Ok(selected)) if selected.variable.index.is_none()
                ),
            }
    }

    /// Whether `symbol` evaluates to itself: nothing the analysis sees binds
    /// it and no builtin object has its name, so as an index it is the key
    /// it spells, `E` of `x_E`.
    fn is_bare_symbol(
        &self,
        symbol: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let position = source.position_for_node(symbol);
        let scope_idx = self.find_scope_at(position).unwrap_or(0);
        symbol.is::<Symbol>()
            && self
                .get_binding_from_scope(symbol.text(), scope_idx, position)
                .is_none()
            && TypeChecker::new(self, knowledge)
                .type_of(symbol, source, scope_idx)
                .single()
                .is_some_and(|class| *class == TypeRole::Symbol.object_name())
    }

    /// The ring `ring` names when its binding holds one the program writes
    /// out.
    fn written_ring(
        &self,
        ring: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&RingValue> {
        let ring = parenthesized_value(ring)?;
        let name = ring.is::<Symbol>().then(|| ring.text())?;
        let position = source.position_for_node(ring);
        let binding =
            self.get_binding_from_scope(name, self.find_scope_at(position).unwrap_or(0), position)?;
        match &binding.state.known_value {
            Some(KnownValue::Ring(value)) => Some(value),
            _ => None,
        }
    }

    /// The name the left operand of a lookup `v_R` gives, with the method
    /// Macaulay2 dispatches it to: a string, a symbol not holding an indexed
    /// variable table (whose `x_R` reads the table at the key `R`), a
    /// selected ring variable, or such a name indexed by a literal.
    fn variable_name(
        &self,
        expression: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<(VariableName, VariableLookup)> {
        let expression = parenthesized_value(expression)?;
        if let Some(text) = expression.string_literal_inner_text() {
            // Escaped strings require evaluation; do not compare their raw spelling.
            return (!text.contains('\\'))
                .then(|| (VariableName(text.to_string()), VariableLookup::String));
        }
        if expression.is::<Symbol>() {
            let position = source.position_for_node(expression);
            let scope_idx = self.find_scope_at(position).unwrap_or(0);
            let lookup = match self.get_binding_from_scope(expression.text(), scope_idx, position) {
                None => VariableLookup::Symbol,
                Some(binding) if binding.state.indexed_element_type.is_some() => return None,
                Some(binding) if binding.state.is_ring_generator => VariableLookup::RingElement,
                Some(_) => return None,
            };
            return Some((VariableName(expression.text().to_string()), lookup));
        }
        if let Some(Ok(selected)) = self.selected_ring_variable(expression, source, knowledge) {
            return Some((
                VariableName(selected.variable.to_string()),
                VariableLookup::RingElement,
            ));
        }
        let parts = expression
            .binary_parts()
            .filter(|_| expression.has_binary_operator::<Token![_]>())?;
        let bare_symbols = |symbol: SyntaxNode<'_>| self.is_bare_symbol(symbol, source, knowledge);
        let index = IndexKey::from_node(parts.right, &bare_symbols)?;
        let (base, lookup) = self.indexed_variable_base(parts.left, &index, source, knowledge)?;
        Some((
            VariableName(format!("{base}_{}", index.subscript())),
            lookup,
        ))
    }

    /// The name `base` gives an indexed variable `base_index`, with what the
    /// indexed variable is: a symbol unbound, holding an indexed variable
    /// table — whose index a ring constructor may have made a ring element —
    /// or holding a ring generator, or a selected unindexed ring variable.
    fn indexed_variable_base(
        &self,
        base: SyntaxNode<'_>,
        index: &IndexKey,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<(VariableName, VariableLookup)> {
        let base = parenthesized_value(base)?;
        if base.is::<Symbol>() {
            let position = source.position_for_node(base);
            let scope_idx = self.find_scope_at(position).unwrap_or(0);
            let lookup = match self.get_binding_from_scope(base.text(), scope_idx, position) {
                None => VariableLookup::IndexedVariable,
                Some(binding) => match &binding.state.indexed_element_type {
                    Some(table) => match table.write_at(index) {
                        None => VariableLookup::IndexedVariable,
                        Some(write) if write.writer == IndexedWriter::RingConstructor => {
                            VariableLookup::RingElement
                        }
                        Some(_) => return None,
                    },
                    None if binding.state.is_ring_generator => VariableLookup::IndexedVariable,
                    None => return None,
                },
            };
            return Some((VariableName(base.text().to_string()), lookup));
        }
        match self.selected_ring_variable(base, source, knowledge)? {
            Ok(selected) if selected.variable.index.is_none() => Some((
                VariableName(selected.variable.base.name().to_string()),
                VariableLookup::IndexedVariable,
            )),
            _ => None,
        }
    }

    /// The ring `expression` evaluates to when the program writes it out: a
    /// constructor over its coefficient ring, a quotient, a named ring
    /// holding one, or a ring certainly adding no variables.
    fn ring_value(
        &self,
        expression: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> Option<RingValue> {
        let expression = parenthesized_value(expression)?;
        if let Some(variables) =
            self.ring_constructor_variables(expression, source, knowledge, scope_idx)
        {
            let coefficient_ring = expression.binary_parts()?.left;
            return Some(RingValue::Polynomial {
                coefficient_ring: self
                    .ring_value(coefficient_ring, source, knowledge, scope_idx)
                    .map(Box::new),
                variables: RingGeneratorBinding::collect(variables, &|symbol| {
                    self.is_bare_symbol(symbol, source, knowledge)
                })
                .iter()
                .map(RingGenerator::from)
                .collect(),
            });
        }
        if expression.has_binary_operator::<Token![/]>() {
            let quotiented = expression.binary_parts()?.left;
            return self
                .ring_value(quotiented, source, knowledge, scope_idx)
                .map(|ring| RingValue::Quotient(Box::new(ring)));
        }
        self.written_ring(expression, source).cloned().or_else(|| {
            self.is_basic_ring(expression, source, knowledge, scope_idx)
                .then_some(RingValue::Basic)
        })
    }

    /// Whether `ring` is certainly a ring of a class with no variables to
    /// select: every class its type admits is a ring neither polynomial nor
    /// quotient, as `RealField` of `RR_53`, exactly `Ring` of `QQ`, or the
    /// family `RR` standing for its default field. A bound such as `↑Ring`
    /// admits polynomial rings, so it is not.
    fn is_basic_ring(
        &self,
        ring: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> bool {
        let ring_type = TypeChecker::new(self, knowledge).type_of(ring, source, scope_idx);
        let variable_classes = VARIABLE_RING_CLASSES.map(ObjectName::new);
        let is_ring = |class: &ObjectName| {
            knowledge.has_type_role(class, TypeRole::Ring)
                || knowledge.is_subtype(class, &ObjectName::new(INEXACT_FIELD_FAMILY))
        };
        let mut exact = ring_type.exact_points().peekable();
        let mut bounds = ring_type.upward_generators().peekable();
        (exact.peek().is_some() || bounds.peek().is_some())
            && exact.all(|class| {
                is_ring(class)
                    && !variable_classes
                        .iter()
                        .any(|variable_class| knowledge.is_subtype(class, variable_class))
            })
            && bounds.all(|bound| {
                is_ring(bound)
                    && !variable_classes.iter().any(|variable_class| {
                        knowledge.is_subtype(bound, variable_class)
                            || knowledge.is_subtype(variable_class, bound)
                    })
            })
    }

    /// The variables a ring constructor spells out as plain symbols — `QQ[x, y]`
    /// yields `x` and `y`. Indexed families (`x_0`, `x_0..x_3`), generated tables
    /// (`Variables => 5`), and letter ranges (`a..d`) are not written as single
    /// symbols and are left out, so callers see exactly the names an author typed.
    pub(crate) fn ring_constructor_symbol_variables<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> Vec<SyntaxNode<'tree>> {
        let Some(variables) = self.ring_constructor_variables(node, source, knowledge, scope_idx)
        else {
            return Vec::new();
        };
        RingGeneratorBinding::collect(variables, &|symbol| {
            self.is_bare_symbol(symbol, source, knowledge)
        })
        .into_iter()
        .filter(|binding| binding.kind == RingGeneratorKind::Direct && binding.node.is::<Symbol>())
        .map(|binding| binding.node)
        .collect()
    }

    /// The variable list of `node` when it constructs a ring, the `[x, y]` of
    /// `QQ[x, y]`: an application of a ring or a family of inexact fields.
    fn ring_constructor_variables<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> Option<SyntaxNode<'tree>> {
        if !node.is_space_application() {
            return None;
        }
        let head = node.binary_parts()?.left;
        let variables = RingGeneratorBinding::constructor_variables(node)?;
        TypeChecker::new(self, knowledge)
            .type_of(head, source, scope_idx)
            .single()
            .is_some_and(|head_type| {
                knowledge.has_type_role(head_type, TypeRole::Ring)
                    || knowledge.is_subtype(head_type, &ObjectName::new(INEXACT_FIELD_FAMILY))
            })
            .then_some(variables)
    }

    /// Bind the generators of the ring constructor `node` when no ring
    /// assignment around it has: Macaulay2 assigns them as soon as the
    /// constructor runs, `QQ[x]` alone making `x` an element of that ring.
    pub fn bind_anonymous_ring_generators(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        let scope_idx = self
            .find_scope_at(source.position_for_node(node))
            .unwrap_or(0);
        let Some(variables) = self.ring_constructor_variables(node, source, knowledge, scope_idx)
        else {
            return;
        };
        let range = source.range_for_node(variables);
        if self.registry.ring_generator_specs.contains(&range) {
            return;
        }
        self.registry.ring_generator_specs.push(range);
        for generator in RingGeneratorBinding::collect(variables, &|symbol| {
            self.is_bare_symbol(symbol, source, knowledge)
        }) {
            self.register_ring_generator(
                ANONYMOUS_RING_ELEMENT,
                &generator.name,
                generator.kind,
                generator.indices,
                generator.node,
                source,
            );
        }
    }

    /// Whether `range` lies in source declaring ring generators: the variable
    /// list of a ring constructor, or the `Variable` option of a field.
    pub(super) fn declares_ring_generators(&self, range: TextRange) -> bool {
        self.registry
            .ring_generator_specs
            .iter()
            .any(|spec| spec.start <= range.start && range.end <= spec.end)
    }

    /// Name of the source binding supplying a ring identity, following parenthesized expressions.
    fn ring_source_symbol<'tree>(&self, expression: SyntaxNode<'tree>) -> Option<&'tree str> {
        let expression = parenthesized_value(expression).unwrap_or(expression);
        if let Some(name) = symbol_node_text(expression) {
            return Some(name);
        }
        if expression.has_binary_operator::<Token![/]>() {
            return expression
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(symbol_node_text);
        }
        None
    }

    /// Whether an indexed access selects a generator recorded by a ring constructor.
    pub(super) fn is_indexed_ring_generator(
        &self,
        node: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> bool {
        self.indexed_ring_generator(node, source).is_some()
    }

    /// The generator an indexed access `z_k` selects, as the binding of its
    /// table and the literal index: the latest write to that index must be a
    /// ring constructor's.
    fn indexed_ring_generator(
        &self,
        node: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<(BindingId, IndexKey)> {
        if !node.has_binary_operator::<Token![_]>() {
            return None;
        }
        let parts = node.binary_parts()?;
        let base = RingGeneratorBinding::name_node(parts.left)?;
        let index = IndexKey::literal(parts.right)?;
        let position = source.position_for_node(base);
        let binding = self.get_binding_from_scope(
            base.text(),
            self.find_scope_at(position).unwrap_or(0),
            position,
        )?;
        binding
            .state
            .indexed_element_type
            .as_ref()?
            .write_at(&index)
            .is_some_and(|write| write.writer == IndexedWriter::RingConstructor)
            .then_some((binding.binding_id, index))
    }

    /// The role of a subscript that is one object spelled with `_`, the same
    /// kind of object as one of its parts, so it reads as a single token:
    /// `3_R`, a number promoted into the ring `R`; `RR_53`, the real field of
    /// that precision. Subscripts selecting something else, as `R_0` selects
    /// a generator, keep their parts; see [`Self::selector_role`].
    pub(super) fn constant_subscript_role(
        &self,
        node: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<SourceSemanticRole> {
        if !node.has_binary_operator::<Token![_]>() {
            return None;
        }
        let parts = node.binary_parts()?;
        if parts.left.is::<IntegerLiteral>() || parts.left.is::<FloatLiteral>() {
            let ring = self.operand_type(
                node,
                parts.right.is::<Symbol>().then_some(parts.right)?,
                source,
                knowledge,
            )?;
            return (knowledge.has_type_role(&ring, TypeRole::Ring)
                || knowledge.is_subtype(&ring, &ObjectName::new(INEXACT_FIELD_FAMILY)))
            .then_some(SourceSemanticRole::RingLiteral);
        }
        if !parts.left.is::<Symbol>() || !parts.right.is::<IntegerLiteral>() {
            return None;
        }
        let base = self.operand_type(node, parts.left, source, knowledge)?;
        knowledge
            .is_subtype(&base, &ObjectName::new(INEXACT_FIELD_FAMILY))
            .then_some(SourceSemanticRole::PrecisionField)
    }

    /// The role of the literal an access `node` selects with, named by what
    /// it selects: a generator for `R_0` on a ring `R`, otherwise a member,
    /// the `0` of `L_0` or `L#0`.
    pub(super) fn selector_role(
        &self,
        node: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> SourceSemanticRole {
        let selects_generator = node.has_binary_operator::<Token![_]>()
            && node
                .binary_parts()
                .and_then(|parts| self.operand_type(node, parts.left, source, knowledge))
                .is_some_and(|base| knowledge.has_type_role(&base, TypeRole::Ring));
        if selects_generator {
            self.ring_generator_role(source.range_for_node(node))
        } else {
            SourceSemanticRole::PropertyKey
        }
    }

    /// The one type inferred for `operand` of the expression `node`.
    fn operand_type(
        &self,
        node: SyntaxNode<'_>,
        operand: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        let scope_idx = self
            .find_scope_at(source.position_for_node(node))
            .unwrap_or(0);
        TypeChecker::new(self, knowledge)
            .type_of(operand, source, scope_idx)
            .single()
            .cloned()
    }

    /// The source of the indexed generator `node` names, from its name on:
    /// `z_0`, also in `symbol z_0`, whose quote is no part of the name.
    pub(super) fn indexed_ring_generator_bytes(node: SyntaxNode<'_>) -> ByteRange {
        let start = node
            .binary_parts()
            .and_then(|parts| RingGeneratorBinding::name_node(parts.left))
            .map_or(node.start_byte(), |base| base.start_byte());
        start..node.end_byte()
    }

    /// The places naming the generator the indexed access at `position`
    /// selects, `z_1` of `QQ[z_0 .. z_2]`: every access to that index of the
    /// table, and the constructor variables declaring it where no access
    /// spells it out, as `z_0 .. z_2` declares `z_1`. `None` when `position`
    /// is not on an indexed generator.
    pub fn indexed_ring_generator_references(
        &self,
        root: SyntaxNode<'_>,
        position: Position,
        source: &(impl SourceNavigation + ?Sized),
        include_declaration: bool,
    ) -> Option<Vec<TextRange>> {
        let point = source.point_for_position(position)?;
        let generator = root
            .descendant_for_point_range(point, point)?
            .enclosing_node(|node| self.is_indexed_ring_generator(*node, source))
            .and_then(|node| self.indexed_ring_generator(node, source))?;
        let mut ranges = root
            .descendants()
            .filter(|node| self.indexed_ring_generator(*node, source).as_ref() == Some(&generator))
            .map(|node| source.range_for_bytes(Self::indexed_ring_generator_bytes(node)))
            .collect::<Vec<_>>();
        let (binding_id, index) = generator;
        let declarations = self.registry.bindings[binding_id.0 as usize]
            .states
            .iter()
            .filter(|state| {
                state.indexed_element_type.as_ref().is_some_and(|elements| {
                    elements.writes.last().is_some_and(|write| {
                        write.writer == IndexedWriter::RingConstructor
                            && write.selection.contains(&index)
                    })
                })
            })
            .map(|state| state.span)
            .filter(|span| {
                !ranges
                    .iter()
                    .any(|range| range.start < span.end && span.start < range.end)
            })
            .collect::<Vec<_>>();
        ranges.extend(declarations);
        if !include_declaration {
            ranges.retain(|range| !self.declares_ring_generators(*range));
        }
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        Some(ranges)
    }

    /// Registers ring generator in the current semantic index.
    fn register_ring_generator(
        &mut self,
        ring_name: &str,
        generator_name: &str,
        kind: RingGeneratorKind,
        indices: Option<IndexSelection>,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        match kind {
            RingGeneratorKind::Direct => {
                self.register_dynamic_global(
                    generator_name,
                    node,
                    ObjectName::new(ring_name),
                    None,
                    source,
                );
            }
            RingGeneratorKind::IndexedTable => {
                let mut elements = self
                    // Several disjoint specs can be rebound at the same anchor
                    // (for example by a quotient). Include earlier writes there.
                    .get_binding_from_scope(generator_name, 0, source.range_for_node(node).end)
                    .and_then(|binding| binding.state.indexed_element_type.clone())
                    .unwrap_or_default();
                if let Some(selection) = indices {
                    elements.writes.push(IndexedWrite {
                        selection,
                        value_type: ObjectName::new(ring_name),
                        writer: IndexedWriter::RingConstructor,
                    });
                }
                self.register_dynamic_global(
                    generator_name,
                    node,
                    ObjectName::new(INDEXED_VARIABLE_TABLE),
                    Some(elements),
                    source,
                );
            }
        }
    }

    /// Record the assignment `target = value` to an indexed variable,
    /// `x_2 = 4`: it makes `x` an `IndexedVariableTable` remembering that
    /// index holds a `value_type`. Only a table, or a name nothing binds, is
    /// written so; Macaulay2 dispatches `x_2 = v` on the class of any other
    /// value of `x`.
    pub fn register_indexed_assignment(
        &mut self,
        target: SyntaxNode,
        value_type: ObjectName,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let Some(parts) = target
            .binary_parts()
            .filter(|_| target.has_binary_operator::<Token![_]>())
        else {
            return;
        };
        let Some(base) = RingGeneratorBinding::name_node(parts.left) else {
            return;
        };
        let position = source.position_for_node(target);
        let visible = self.get_binding_from_scope(
            base.text(),
            self.find_scope_at(position).unwrap_or(0),
            position,
        );
        if visible.is_some_and(|binding| {
            binding.scope_idx != 0 || binding.state.indexed_element_type.is_none()
        }) {
            return;
        }
        let mut writes = visible
            .and_then(|binding| binding.state.indexed_element_type.clone())
            .unwrap_or_default();
        writes.writes.push(IndexedWrite {
            selection: IndexKey::literal(parts.right)
                .map_or(IndexSelection::Unknown, IndexSelection::Exact),
            value_type,
            writer: IndexedWriter::Assignment,
        });
        self.register_dynamic_global(
            base.text(),
            target,
            ObjectName::new(INDEXED_VARIABLE_TABLE),
            Some(writes),
            source,
        );
    }

    /// Registers dynamic global in the current semantic index.
    fn register_dynamic_global(
        &mut self,
        name: &str,
        node: SyntaxNode,
        type_name: ObjectName,
        indexed_element_type: Option<IndexedVariableTypes>,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let position = source.position_for_node(node);
        let binding_id = self
            .binding_id_from_scope(name, 0, position)
            .filter(|binding_id| {
                self.binding_anchor(*binding_id)
                    .is_some_and(|binding| binding.scope_idx == 0)
            })
            .or_else(|| self.future_assignment_binding(name, 0, position));
        let fact = BindingFact {
            name: ObjectName::new(name),
            ..BindingFact::from_node(
                node,
                None,
                BindingRole::Ordinary,
                BindingEffect::Assign,
                0,
                true,
                source,
            )
        };
        let enrichment = BindingEnrichment {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: Some(InferredType::exact_from_id(type_name)),
            object_id: None,
            indexed_element_type,
            parent_type: None,
        };
        let (binding_id, state_index) = if let Some(binding_id) = binding_id {
            let Some(mut state_index) = self.add_binding_state(binding_id, fact) else {
                return;
            };
            if let Some(binding) = self.registry.bindings.get_mut(binding_id.0 as usize) {
                let range = source.range_for_node(node);
                if range.start < binding.range.start {
                    binding.range = range;
                    let state = binding.states.remove(state_index);
                    binding.states.insert(0, state);
                    state_index = 0;
                }
            }
            self.reindex_binding_states(binding_id);
            (binding_id, state_index)
        } else {
            (self.add_binding(fact), 0)
        };
        let is_ring_generator = enrichment.indexed_element_type.is_none();
        self.enrich_binding_state(binding_id, state_index, enrichment);
        self.registry.bindings[binding_id.0 as usize].states[state_index].is_ring_generator =
            is_ring_generator;
    }
}

impl<'tree> RingGeneratorBinding<'tree> {
    /// Variable specification supplied by a recognized polynomial-ring constructor.
    fn constructor_variables(application: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let argument = parenthesized_value(application.binary_parts().map(|parts| parts.right)?)?;
        let is_constructor_argument =
            |node: &SyntaxNode<'tree>| node.is::<Array>() || node.is::<List>();
        if is_constructor_argument(&argument) {
            return Some(argument);
        }
        argument
            .binary_parts()
            .map(|parts| parts.left)
            .filter(is_constructor_argument)
    }

    /// Collects ring-generator specifications, retaining the selected policy for bare symbols.
    fn collect(container: SyntaxNode<'tree>, bare_symbols: BareSymbols<'_>) -> Vec<Self> {
        let elements = container.collection_elements().collect::<Vec<_>>();
        let variable_base = elements
            .iter()
            .find_map(|element| Self::option_value(*element, "VariableBaseName"))
            .and_then(Self::base_name);
        let mut bindings = Vec::new();

        for element in elements {
            if element.is_option_assignment() {
                if let Some(variables) = Self::option_value(element, "Variables") {
                    if variables.is::<IntegerLiteral>() {
                        let name = variable_base.clone().unwrap_or_else(|| "p".to_string());
                        Self::push(
                            &mut bindings,
                            RingGeneratorBinding {
                                name,
                                kind: RingGeneratorKind::IndexedTable,
                                node: variables,
                                indices: variables
                                    .text()
                                    .parse::<i64>()
                                    .ok()
                                    .filter(|n| *n > 0)
                                    .map(|n| IndexSelection::Range {
                                        start: IndexKey::Integer(0),
                                        end: IndexKey::Integer(n - 1),
                                        exclusive: false,
                                    }),
                            },
                        );
                    } else {
                        Self::collect_spec(variables, &mut bindings, bare_symbols);
                    }
                }
                continue;
            }
            Self::collect_spec(element, &mut bindings, bare_symbols);
        }

        bindings
    }

    /// Value of the named option in a recognized option assignment.
    fn option_value(node: SyntaxNode<'tree>, key: &str) -> Option<SyntaxNode<'tree>> {
        if !node.is_option_assignment() {
            return None;
        }
        node.binary_parts()
            .map(|parts| parts.left)
            .filter(|left| left.is::<Symbol>() && left.text() == key)?;
        node.binary_parts().map(|parts| parts.right)
    }

    /// Base symbol name of a generator, excluding a recognized index selection.
    fn base_name(node: SyntaxNode<'tree>) -> Option<String> {
        if node.is::<Symbol>() {
            Some(node.text().to_string())
        } else {
            node.string_literal_inner_text().map(ToString::to_string)
        }
    }

    /// The generators named by the `Variable` option of the outermost call
    /// of `expression`, the `g` of `GF(8, Variable => g)`, with that option's
    /// value.
    fn variable_option(
        expression: SyntaxNode<'tree>,
        bare_symbols: BareSymbols<'_>,
    ) -> Option<(SyntaxNode<'tree>, Vec<Self>)> {
        let application = parenthesized_value(expression)?;
        if !application.is_space_application() {
            return None;
        }
        let arguments = parenthesized_value(application.binary_parts()?.right)?;
        let value = arguments
            .collection_elements()
            .find_map(|argument| Self::option_value(argument, VARIABLE_OPTION))?;
        let mut bindings = Vec::new();
        Self::collect_spec(value, &mut bindings, bare_symbols);
        Some((value, bindings))
    }

    /// The name `node` gives a generator, unquoted: `x`, `symbol x`,
    /// `local x`, or `(x)`.
    fn name_node(node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let node = parenthesized_value(node)?;
        Some(node.quoted_symbol().unwrap_or(node)).filter(|node| node.is::<Symbol>())
    }

    /// The string of `getSymbol "x"`, which names the symbol `x`.
    fn get_symbol_argument(node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let parts = node
            .binary_parts()
            .filter(|_| node.is_space_application())?;
        (parts.left.is::<Symbol>() && parts.left.text() == GET_SYMBOL_FUNCTION).then_some(())?;
        parenthesized_value(parts.right)
            .filter(|argument| argument.string_literal_inner_text().is_some())
    }

    /// Collects spec from the supplied syntax and semantic context.
    fn collect_spec(
        node: SyntaxNode<'tree>,
        bindings: &mut Vec<RingGeneratorBinding<'tree>>,
        bare_symbols: BareSymbols<'_>,
    ) {
        let Some(node) = parenthesized_value(node) else {
            return;
        };
        if let Some(argument) = Self::get_symbol_argument(node) {
            if let Some(name) = argument.string_literal_inner_text() {
                Self::push(
                    bindings,
                    RingGeneratorBinding {
                        name: name.to_string(),
                        kind: RingGeneratorKind::Direct,
                        node: argument,
                        indices: None,
                    },
                );
            }
            return;
        }
        let node = Self::name_node(node).unwrap_or(node);
        if node.is::<Symbol>() {
            Self::push(
                bindings,
                RingGeneratorBinding {
                    name: node.text().to_string(),
                    kind: RingGeneratorKind::Direct,
                    node,
                    indices: None,
                },
            );
            return;
        }

        if node.has_binary_operator::<Token![_]>() {
            if let Some(base) = node
                .binary_parts()
                .and_then(|parts| Self::name_node(parts.left))
            {
                Self::push(
                    bindings,
                    RingGeneratorBinding {
                        name: base.text().to_string(),
                        kind: RingGeneratorKind::IndexedTable,
                        node: base,
                        indices: node
                            .binary_parts()
                            .and_then(|parts| IndexKey::from_node(parts.right, bare_symbols))
                            .map(IndexSelection::Exact),
                    },
                );
            }
            return;
        }

        if node.has_binary_operator::<Token![..]>() || node.has_binary_operator::<Token![..<]>() {
            let left = node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(parenthesized_value);
            let right = node
                .binary_parts()
                .map(|parts| parts.right)
                .and_then(parenthesized_value);
            if let (Some(left), Some(right)) = (left, right) {
                if let (Some(left_base), Some(right_base)) =
                    (Self::indexed_base(left), Self::indexed_base(right))
                {
                    let indices = left
                        .binary_parts()
                        .and_then(|parts| IndexKey::from_node(parts.right, bare_symbols))
                        .zip(
                            right
                                .binary_parts()
                                .and_then(|parts| IndexKey::from_node(parts.right, bare_symbols)),
                        )
                        .map(|(start, end)| IndexSelection::Range {
                            start,
                            end,
                            exclusive: node.has_binary_operator::<Token![..<]>(),
                        });
                    if let Some(names) = Self::symbol_range(node, left_base, right_base) {
                        for name in names {
                            Self::push(
                                bindings,
                                RingGeneratorBinding {
                                    name,
                                    kind: RingGeneratorKind::IndexedTable,
                                    node,
                                    indices: indices.clone(),
                                },
                            );
                        }
                    }
                    return;
                }

                if let Some(names) = Self::name_node(left)
                    .zip(Self::name_node(right))
                    .and_then(|(left, right)| Self::symbol_range(node, left, right))
                {
                    for name in names {
                        Self::push(
                            bindings,
                            RingGeneratorBinding {
                                name,
                                kind: RingGeneratorKind::Direct,
                                node,
                                indices: None,
                            },
                        );
                    }
                    return;
                }
            }
        }

        if node.is_collection_expression() {
            for element in node.collection_elements() {
                Self::collect_spec(element, bindings, bare_symbols);
            }
        }
    }

    /// Base expression of a subscripted variable specification.
    fn indexed_base(node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let node = parenthesized_value(node)?;
        node.has_binary_operator::<Token![_]>()
            .then(|| node.binary_parts().map(|parts| parts.left))
            .flatten()
            .and_then(Self::name_node)
    }

    /// Expands a statically known range of generator names into its individual symbols.
    fn symbol_range(
        range: SyntaxNode<'tree>,
        left: SyntaxNode<'tree>,
        right: SyntaxNode<'tree>,
    ) -> Option<Vec<String>> {
        if !left.is::<Symbol>() || !right.is::<Symbol>() {
            return None;
        }
        let names = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        // Equal endpoints also support names outside the standard vars alphabet.
        if left.text() == right.text() {
            return Some(if range.has_binary_operator::<Token![..<]>() {
                Vec::new()
            } else {
                vec![left.text().to_string()]
            });
        }
        let start = names.chars().position(|c| c.to_string() == left.text())?;
        let end = names.chars().position(|c| c.to_string() == right.text())?;
        let stop = end + usize::from(!range.has_binary_operator::<Token![..<]>());
        Some(
            names
                .chars()
                .skip(start)
                .take(stop.saturating_sub(start))
                .map(|c| c.to_string())
                .collect(),
        )
    }

    /// Adds a ring-generator binding unless an equivalent binding is already present.
    fn push(bindings: &mut Vec<RingGeneratorBinding<'tree>>, binding: RingGeneratorBinding<'tree>) {
        if !bindings.iter().any(|existing| {
            existing.name == binding.name
                && existing.kind == binding.kind
                && existing.indices == binding.indices
        }) {
            bindings.push(binding);
        }
    }
}

impl<Knowledge: PositionedTypeKnowledge + ?Sized> TypeChecker<'_, '_, Knowledge> {
    /// Infer the ring-specific indexed-variable, precision-field, and quotient
    /// operator cases.
    pub fn ring_operator_type(
        &self,
        query: OperatorTypeQuery<'_>,
        left_name: &ObjectName,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let promoted_value = parenthesized_value(query.left)?;
        // `InexactFieldFamily _ ZZ` is the only subscript of a family, so
        // `RR_p` is the real field of precision `p` whatever `p` is.
        if matches_token::<Token![_]>(query.operator)
            && knowledge.is_subtype(left_name, &ObjectName::new(INEXACT_FIELD_FAMILY))
        {
            let family = symbol_node_text(promoted_value);
            let field = PRECISION_FIELD_CLASSES
                .iter()
                .find(|(name, _)| family == Some(*name))
                .map_or(INEXACT_FIELD, |(_, field)| field);
            return Some(InferredType::exact(field));
        }

        if matches_token::<Token![_]>(query.operator)
            && (promoted_value.is::<IntegerLiteral>() || promoted_value.is::<FloatLiteral>())
        {
            let ring = parenthesized_value(query.right?)?;
            let ring_type = self.type_of(ring, source, query.scope_idx);
            if knowledge.has_type_role(ring_type.single()?, TypeRole::Ring) {
                let element_type = self
                    .resolved_ring_element_type(
                        ring,
                        source,
                        query.scope_idx,
                        knowledge,
                        &mut HashSet::new(),
                    )
                    .unwrap_or_else(|| ObjectName::new("RingElement"));
                return Some(InferredType::exact_from_id(element_type));
            }
        }

        if matches_token::<Token![_]>(query.operator)
            && left_name.as_ref() == INDEXED_VARIABLE_TABLE
        {
            if let Some(elements) = self
                .get_binding_from_scope(
                    query.left.text(),
                    query.scope_idx,
                    source.position_for_node(query.left),
                )
                .and_then(|binding| binding.state.indexed_element_type.as_ref())
            {
                if let Some(index) = query.right.and_then(IndexKey::literal) {
                    return Some(elements.read_type(&index));
                }
                if query
                    .right
                    .is_some_and(|key| self.written_ring(key, source).is_some())
                {
                    return Some(elements.read_nonliteral_type());
                }
                return Some(
                    elements.writes.iter().fold(
                        InferredType::exact(INDEXED_VARIABLE),
                        |types, write| {
                            types | InferredType::exact_from_id(write.value_type.clone())
                        },
                    ),
                );
            }
            return Some(InferredType::exact(INDEXED_VARIABLE));
        }

        if matches_token::<Token![/]>(query.operator)
            && knowledge.has_type_role(left_name, TypeRole::Ring)
        {
            let right_type = self.type_of(query.right?, source, query.scope_idx);
            if right_type.single()?.as_ref() == "ZZ" {
                return Some(InferredType::exact("QuotientRing"));
            }
        }

        None
    }

    /// Element class associated with the selected ring under the current source knowledge.
    pub(super) fn resolved_ring_element_type(
        &self,
        ring: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
        visited: &mut HashSet<BindingId>,
    ) -> Option<ObjectName> {
        let ring = parenthesized_value(ring)?;
        let name = symbol_node_text(ring)?;
        let position = source.position_for_node(ring);
        let Some(binding) =
            self.visible_source_binding_from_scope(name, scope_idx, position, knowledge)
        else {
            let name = ObjectName::new(name);
            self.resolve_type_id_at(&name, position, knowledge)?;
            return Some(name);
        };
        if !visited.insert(binding.binding_id) {
            return None;
        }

        let value_range = binding.state.value_range?;
        let root = ring.root();
        let value = root.descendant_for_point_range(
            source.point_for_position(value_range.start)?,
            source.point_for_position(value_range.end)?,
        )?;
        if source.range_for_node(value) != value_range {
            return None;
        }
        let value_scope_idx = self
            .find_scope_at(source.position_for_node(value))
            .unwrap_or(binding.state.scope_idx);
        if symbol_node_text(parenthesized_value(value)?).is_some() {
            return self.resolved_ring_element_type(
                value,
                source,
                value_scope_idx,
                knowledge,
                visited,
            );
        }

        (binding.scope_idx == 0
            && self.expression_introduces_ring_identity(value, source, value_scope_idx, knowledge))
        .then(|| binding.name.clone())
        .filter(|_| binding.state.source_type.is_some())
    }

    /// Whether evaluating this expression creates a distinct ring rather than forwarding an existing one.
    fn expression_introduces_ring_identity(
        &self,
        expression: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let Some(expression) = parenthesized_value(expression) else {
            return false;
        };
        if expression.is::<NewStatement>() {
            return true;
        }
        let introduces_identity =
            expression.is_space_application() || expression.has_binary_operator::<Token![/]>();
        let operand = introduces_identity
            .then(|| expression.binary_parts().map(|parts| parts.left))
            .flatten();
        operand.is_some_and(|operand| {
            self.type_of(operand, source, scope_idx)
                .single()
                .is_some_and(|type_name| {
                    knowledge.has_type_role(type_name, TypeRole::Ring)
                        || knowledge.is_subtype(type_name, &ObjectName::new(INEXACT_FIELD_FAMILY))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;
    use std::sync::OnceLock;

    fn document(source: &str) -> DocumentSnapshot {
        static BUILTINS: OnceLock<ObjectRegistry> = OnceLock::new();
        let builtins = BUILTINS.get_or_init(|| ObjectRegistry::load(crate::catalog::test_corpus()));
        DocumentSnapshot::from_text(source.to_string(), builtins).expect("fixture should parse")
    }

    /// Check complete expressions, including indexed accesses, rather than the
    /// types of their symbol children or the presence of an unrelated inlay hint.
    fn assert_types(setup: &str, probes: &[(&str, &str)]) {
        let source = format!(
            "{setup}\n{}\n",
            probes
                .iter()
                .map(|(expr, _)| *expr)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let document = document(&source);
        assert!(
            !document.root_node().has_error(),
            "invalid fixture: {source}"
        );
        let checker = TypeChecker::new(document.analysis(), document.object_registry());
        let first_line = setup.lines().count();
        for (offset, (expression, expected)) in probes.iter().enumerate() {
            let line = (first_line + offset) as u32;
            let node = document
                .root_node()
                .descendants()
                .filter(|node| {
                    node.text() == *expression && document.position_for_node(*node).line == line
                })
                .last()
                .unwrap_or_else(|| panic!("missing probe {expression:?} on line {line}: {source}"));
            let inferred = checker.type_of(node, &document, 0);
            assert_eq!(
                inferred.single().map(ObjectName::name),
                Some(*expected),
                "wrong type for {expression:?} after {setup:?}: {inferred:?}"
            );
        }
    }

    /// Check what each subscript `(line, access)` of `source` evaluates to:
    /// the variable as Macaulay2 prints it, Macaulay2's error prefixed by
    /// `error: `, or `None` when the program does not determine it.
    fn assert_ring_subscripts(source: &str, probes: &[(u32, &str, Option<&str>)]) {
        let document = document(source);
        assert!(
            !document.root_node().has_error(),
            "invalid fixture: {source}"
        );
        for (line, access, expected) in probes {
            let node = document
                .root_node()
                .descendants()
                .find(|node| {
                    node.text() == *access && document.position_for_node(*node).line == *line
                })
                .unwrap_or_else(|| panic!("missing probe {access:?} on line {line}: {source}"));
            let evaluated = document
                .analysis()
                .selected_ring_variable(node, &document, document.object_registry())
                .map(|selected| match selected {
                    Ok(selected) => selected.variable.to_string(),
                    Err(error) => format!("error: {error}"),
                });
            assert_eq!(
                evaluated.as_deref(),
                *expected,
                "wrong value of {access:?} on line {line}: {source}"
            );
        }
    }

    /// Expected values and errors are those of Macaulay2 1.26.05.
    #[test]
    fn subscripts_select_ring_variables_in_constructor_order() {
        assert_ring_subscripts(
            "R = RR_7\nS = R[x_0..y_2, z, w_(1,1)]\nS_0\nS_5\nS_6\nS_7\nS_8\nS_(-1)\nS_(-9)\n",
            &[
                (2, "S_0", Some("x_0")),
                (3, "S_5", Some("y_2")),
                (4, "S_6", Some("z")),
                (5, "S_7", Some("w_(1,1)")),
                (6, "S_8", Some("error: index 8 out of bounds 0 .. 7")),
                (7, "S_(-1)", Some("w_(1,1)")),
                (8, "S_(-9)", Some("error: index -9 out of bounds 0 .. 7")),
            ],
        );
        assert_ring_subscripts(
            "X = QQ[x_(0,0)..x_(1,2), c_3..c_1, a..c]\nX_4\nX_6\nX_8\nW = QQ[Variables => 2]\nW_1\n",
            &[
                (1, "X_4", Some("x_(1,1)")),
                (2, "X_6", Some("a")),
                (3, "X_8", Some("c")),
                (5, "W_1", Some("p_1")),
            ],
        );
    }

    /// Over a polynomial ring the program writes out, subscripts go on into
    /// its variables; over any other ring they end with the ring's own.
    #[test]
    fn subscripts_reach_into_written_coefficient_rings_only() {
        assert_ring_subscripts(
            "R = QQ[a]\nS = R[x]\nS_1\nS_2\nM = S/ideal(x^2)\nM_1\nM_(-1)\nZ = QQ[y_1..y_2][t]\nZ_2\n",
            &[
                (2, "S_1", Some("a")),
                (3, "S_2", Some("error: index 2 out of bounds 0 .. 1")),
                (5, "M_1", Some("a")),
                (6, "M_(-1)", Some("a")),
                (8, "Z_2", Some("y_2")),
            ],
        );
        assert_ring_subscripts(
            "F = RR_7\nT = F[x]\nT_0\nT_1\nB = GF(8, Variable => g)[y]\nB_1\nP = (ZZ/101)[u]\nP_1\n",
            &[
                (2, "T_0", Some("x")),
                (3, "T_1", Some("error: index 1 out of bounds 0 .. 0")),
                (5, "B_1", Some("error: index 1 out of bounds 0 .. 0")),
                (7, "P_1", Some("error: index 1 out of bounds 0 .. 0")),
            ],
        );
    }

    /// A subscript reads the ring its name holds there; past the variables
    /// the program determines, nothing is claimed, neither variable nor error.
    #[test]
    fn subscripts_follow_reassignment_and_stop_at_undetermined_variables() {
        assert_ring_subscripts(
            "S = QQ[a]\nS_0\nS = QQ[b]\nS_0\ni = random 10\nU = QQ[u, v_i]\nU_0\nU_1\nU_(-1)\nf = R -> (V = R[t]; V_1)\nF = ZZ/101\nF_0\n",
            &[
                (1, "S_0", Some("a")),
                (3, "S_0", Some("b")),
                (6, "U_0", Some("u")),
                (7, "U_1", None),
                (8, "U_(-1)", None),
                (9, "V_1", None),
                (11, "F_0", None),
            ],
        );
    }

    /// `v_R` looks the variable `v` names up in `R` by its printed name,
    /// unless `v` holds an indexed variable table, whose `x_R` reads the
    /// table at the key `R`. Values and classes are those of Macaulay2.
    #[test]
    fn lookups_by_name_select_variables_of_the_target_ring() {
        let setup = "A = RR[x, y_1, x_1]\nB = RR[x, y_2]";
        let probes = [
            ("x", None, "B"),
            ("x_B", Some("x"), "B"),
            ("x_A", Some("x"), "A"),
            ("x_1", None, "IndexedVariable"),
            ("x_1_A", Some("x_1"), "A"),
            ("y", None, "IndexedVariableTable"),
            ("y_1", None, "A"),
            ("y_2", None, "B"),
            ("A_0_B", Some("x"), "B"),
            ("B_0_A", Some("x"), "A"),
        ];
        assert_types(
            setup,
            &probes.map(|(expression, _, class)| (expression, class)),
        );
        let source = format!(
            "{setup}\n{}\n",
            probes.map(|(expression, _, _)| expression).join("\n")
        );
        assert_ring_subscripts(
            &source,
            &probes
                .iter()
                .enumerate()
                .map(|(offset, (expression, value, _))| (offset as u32 + 2, *expression, *value))
                .collect::<Vec<_>>(),
        );
    }

    /// Names come from strings, indexed variables, and selected variables
    /// indexed again: `"x"_A_1` is the indexed variable `x_1`, which `_A`
    /// looks up. `x_A` reads the table `x` at the key `A`.
    #[test]
    fn lookup_names_compose_through_strings_indices_and_selections() {
        let setup = "A = RR[x, y_1, x_1, x_3]\nB = RR[x_1, y_2, x_2]";
        let probes = [
            ("x_A", None, "IndexedVariable"),
            ("x_1_B_A", Some("x_1"), "A"),
            ("x_1_A", Some("x_1"), "A"),
            ("\"x\"_A_1", None, "IndexedVariable"),
            ("\"x\"_A_1_A", Some("x_1"), "A"),
            ("\"x_1\"_A", Some("x_1"), "A"),
            ("A_0_1", None, "IndexedVariable"),
            ("A_0_1_A", Some("x_1"), "A"),
            ("x_3_A", Some("x_3"), "A"),
        ];
        assert_types(
            setup,
            &probes.map(|(expression, _, class)| (expression, class)),
        );
        let source = format!(
            "{setup}\n{}\n",
            probes.map(|(expression, _, _)| expression).join("\n")
        );
        assert_ring_subscripts(
            &source,
            &probes
                .iter()
                .enumerate()
                .map(|(offset, (expression, value, _))| (offset as u32 + 2, *expression, *value))
                .collect::<Vec<_>>(),
        );
    }

    /// An index may be a symbol evaluating to itself, or an indexed variable
    /// of one, printed in parentheses: `x_(E_F)`.
    #[test]
    fn symbolic_indices_name_variables_as_macaulay2_prints_them() {
        let setup = "R = RR[x_(E_F), x]";
        let probes = [
            ("x", None, "R"),
            ("x_(E_F)", None, "IndexedVariable"),
            ("\"x_(E_F)\"_R", Some("x_(E_F)"), "R"),
            ("x_R_(E_F)", None, "IndexedVariable"),
            ("x_R_(E_F)_R", Some("x_(E_F)"), "R"),
            ("x_(E_F)_R", Some("x_(E_F)"), "R"),
            ("R_0", Some("x_(E_F)"), "R"),
            ("R_1", Some("x"), "R"),
        ];
        assert_types(
            setup,
            &probes.map(|(expression, _, class)| (expression, class)),
        );
        let source = format!(
            "{setup}\n{}\n",
            probes.map(|(expression, _, _)| expression).join("\n")
        );
        assert_ring_subscripts(
            &source,
            &probes
                .iter()
                .enumerate()
                .map(|(offset, (expression, value, _))| (offset as u32 + 1, *expression, *value))
                .collect::<Vec<_>>(),
        );
        assert_ring_subscripts("E = 2\nS = RR[y_E]\nS_0\n", &[(2, "S_0", None)]);
    }

    /// A missing name is an error for a string or an indexed variable; a
    /// ring element is promoted instead, and a bare symbol may be bound
    /// where the analysis does not see, so neither is claimed. Lookups
    /// search the whole tower, as subscripts do.
    #[test]
    fn lookups_report_missing_names_only_where_macaulay2_must_fail() {
        assert_ring_subscripts(
            "A = RR[x, y_1, x_1, x_3]\nB = RR[x_1, y_2, x_2]\n\"y\"_A\n\"x_3\"_B\ny_1_B\nx_2_A\nq_A\n",
            &[
                (2, "\"y\"_A", Some("error: variable not found in ring: `y`")),
                (3, "\"x_3\"_B", Some("error: variable not found in ring: `x_3`")),
                (4, "y_1_B", None),
                (5, "x_2_A", None),
                (6, "q_A", None),
            ],
        );
        assert_ring_subscripts(
            "K = QQ[k, m_1]\nS = K[b]\nQ = S/ideal(b^2)\nk_S\n\"m_1\"_Q\nS_1_K\n\"b\"_K\n",
            &[
                (3, "k_S", Some("k")),
                (4, "\"m_1\"_Q", Some("m_1")),
                (5, "S_1_K", Some("k")),
                (6, "\"b\"_K", Some("error: variable not found in ring: `b`")),
            ],
        );
    }

    #[test]
    fn polynomial_constructors_recognize_coefficient_rings_and_spacing() {
        for constructor in [
            "QQ[x]",
            "ZZ[x]",
            "(ZZ/101)[x]",
            "ZZ/101[x]",
            "(QQ)[x]",
            "((QQ))[x]",
            "QQ [x]",
            "QQ\t[x]",
            "QQ[ x , y ]",
            "QQ([x])",
            "QQ[(((x)))]",
            "QQ[(x,y)]",
            "QQ[\r\nx,\r\ny\r\n]",
            "QQ{ x, y }",
            "QQ[{x,y}]",
            "QQ[\n -- generators\n x,\n y\n]",
        ] {
            assert_types(
                &format!("R = {constructor}"),
                &[("R", "PolynomialRing"), ("x", "R")],
            );
        }
    }

    #[test]
    fn inexact_field_families_subscript_into_fields_of_that_precision() {
        assert_types(
            "p = 100",
            &[
                ("RR_53", "RealField"),
                ("RR_p", "RealField"),
                ("CC_53", "ComplexField"),
                ("CC_p", "ComplexField"),
                ("RRi_53", "InexactField"),
                ("RRi_p", "InexactField"),
                ("CCi_53", "InexactField"),
                ("CCi_p", "InexactField"),
            ],
        );
        for field in ["RR_53", "CC_p", "RRi_(2*p)", "CCi_p"] {
            assert_types(
                &format!("p = 100\nR = {field}[x]"),
                &[("R", "PolynomialRing"), ("x", "R")],
            );
        }
    }

    #[test]
    fn polynomial_constructors_recognize_options_and_empty_variable_sets() {
        for constructor in [
            "QQ[]",
            "QQ{}",
            "QQ[{}]",
            "QQ[Variables => 0]",
            "QQ[x, Degrees => {2}]",
            "QQ[x, MonomialOrder => Lex]",
            "QQ[MonomialOrder => Lex, x]",
            "QQ[Variables => {x,y}]",
            "QQ[Variables => [x,y]]",
            "QQ[Variables => 3]",
            "QQ[Variables => 3, VariableBaseName => v]",
            "QQ[VariableBaseName => \"v\", Variables => 3]",
        ] {
            assert_types(&format!("R = {constructor}"), &[("R", "PolynomialRing")]);
        }
        assert_types("R = QQ[Variables => {x,y}]", &[("x", "R"), ("y", "R")]);
        assert_types(
            "R = QQ[Variables => 2]",
            &[
                ("p", "IndexedVariableTable"),
                ("p_0", "R"),
                ("p_1", "R"),
                ("p_2", "IndexedVariable"),
            ],
        );
        assert_types(
            "R = QQ[VariableBaseName => \"v\", Variables => 2]",
            &[("v_1", "R"), ("v_2", "IndexedVariable")],
        );
    }

    #[test]
    fn polynomial_constructors_follow_aliases_and_existing_ring_heads() {
        for setup in [
            "P = QQ\nR = P[x]",
            "P := QQ\nR := P[x]",
            "P = QQ\nA = P\nR = A[x]",
            "P = 1\nP = QQ\nR = P[x]",
            "P = QQ[t]\nR = P[x]",
            "P = QQ[t]\nI = ideal(t^2)\nA = P/I\nR = A[x]",
            "R = QQ[t][x]",
            "R = (QQ[t])[x]",
            "R = (QQ[x])",
            "R = ((QQ[x]))",
        ] {
            assert_types(setup, &[("R", "PolynomialRing"), ("x", "R")]);
        }
    }

    #[test]
    fn polynomial_constructors_preserve_trailing_operator_dispatch() {
        for setup in [
            "I = ideal(0)\nR = QQ[x]/I",
            "I = ideal(0)\nR = (QQ[x])/I",
            "I = ideal(0)\nR = QQ[x] / I",
            "I = ideal(0)\nR = (QQ[x]/I)",
            "A = QQ[t]\nI = ideal(t^2)\nR = A[x]/I",
        ] {
            assert_types(setup, &[("R", "QuotientRing"), ("x", "R")]);
        }
        assert_types("M = QQ[x]^2", &[("M", "Module")]);
    }

    #[test]
    fn polynomial_constructor_detection_does_not_confuse_other_applications() {
        // Brackets alone, a familiar name, or a Ring result from an ordinary
        // function call must not turn its arguments into ring generators.
        for setup in [
            "R = [x,y]",
            "R = {x,y}",
            "R = (x,y)",
            "f = args -> args\nR = f[x]",
            "f = args -> args\nR = f{x}",
            "f = args -> QQ\nR = f[x]",
            "R = unknownHead[x]",
            "R = P[x]\nP = QQ",
            "P = 1\nR = P[x]\nP = QQ",
            "R = QQ(x,y)",
            "R = QQ<|x|>",
            "P = 7\nR = P[x]",
            "P = \"QQ\"\nR = P[x]",
            "P = QQ\nP = 7\nR = P[x]",
            "QQ = 7\nR = QQ[x]",
            "R = \"QQ[x]\"",
            "-- R = QQ[x]\nR = 1",
        ] {
            assert_types(setup, &[("x", "Symbol")]);
        }
    }

    #[test]
    fn polynomial_constructor_symbol_detection_uses_the_head_type_and_scope() {
        for (source, expected) in [
            ("R = QQ[longName]", vec!["longName"]),
            ("R = QQ[((longName))]", vec!["longName"]),
            ("R = QQ([longName])", vec!["longName"]),
            ("P = QQ\nR = P[longName]", vec!["longName"]),
            ("P = QQ\nf = () -> P[longName]", vec!["longName"]),
            ("f = () -> (P := QQ; P[longName])", vec!["longName"]),
            ("R = (ZZ/101)[longName]", vec!["longName"]),
            ("R = QQ[longName]/ideal(0)", vec!["longName"]),
            ("R = QQ[x]/ideal(x^2)", vec!["x"]),
            (
                "R = QQ[longName, otherName, x_1, a..c]",
                vec!["longName", "otherName"],
            ),
            ("R = QQ[Degrees => {longName}, x]", vec!["x"]),
            ("R = QQ[Variables => {longName, x_1}]", vec!["longName"]),
            ("R = QQ[x_1..y_3, Variables => 2]", vec![]),
            ("R = [longName]", vec![]),
            ("R = QQ(longName, otherName)", vec![]),
            ("R = QQ<|longName|>", vec![]),
            ("R = unknownHead[longName]", vec![]),
            ("f = x -> x\nR = f[longName]", vec![]),
            ("P = QQ\nf = P -> P[longName]", vec![]),
            ("P = QQ\nf = () -> (P := 1; P[longName])", vec![]),
            ("P = QQ\nP = 1\nR = P[longName]", vec![]),
        ] {
            let document = document(source);
            let found = document
                .root_node()
                .descendants()
                .flat_map(|node| {
                    let scope = document
                        .analysis()
                        .find_scope_at(document.position_for_node(node))
                        .unwrap_or(0);
                    document.analysis().ring_constructor_symbol_variables(
                        node,
                        &document,
                        document.object_registry(),
                        scope,
                    )
                })
                .map(|node| node.text().to_string())
                .collect::<Vec<_>>();
            assert_eq!(found, expected, "wrong constructor symbols in {source:?}");
        }
    }

    #[test]
    fn polynomial_generator_ranges_cover_boundaries_and_index_shapes() {
        for (constructor, probes) in [
            (
                "QQ[a..c]",
                vec![("a", "R"), ("b", "R"), ("c", "R"), ("d", "Symbol")],
            ),
            ("QQ[a..<d]", vec![("c", "R"), ("d", "Symbol")]),
            (
                "QQ[z..B]",
                vec![("z", "R"), ("A", "R"), ("B", "R"), ("C", "Symbol")],
            ),
            ("QQ[longName..longName]", vec![("longName", "R")]),
            ("QQ[c..a]", vec![("a", "Symbol"), ("c", "Symbol")]),
            (
                "QQ[((x)_1)]",
                vec![
                    ("x", "IndexedVariableTable"),
                    ("x_1", "R"),
                    ("x_2", "IndexedVariable"),
                ],
            ),
            (
                "QQ[(x_1)..(x_3)]",
                vec![("x_1", "R"), ("x_3", "R"), ("x_4", "IndexedVariable")],
            ),
            (
                "QQ[x_1, x_3]",
                vec![("x_1", "R"), ("x_2", "IndexedVariable"), ("x_3", "R")],
            ),
            (
                "QQ[x_(-2)..x_1]",
                vec![
                    ("x_(-3)", "IndexedVariable"),
                    ("x_(-2)", "R"),
                    ("x_0", "R"),
                    ("x_2", "IndexedVariable"),
                ],
            ),
            (
                "QQ[x_{1,2}]",
                vec![
                    ("x_{1,2}", "R"),
                    ("x_{2,1}", "IndexedVariable"),
                    ("x_(1,2)", "IndexedVariable"),
                ],
            ),
            (
                "QQ[x_(1,2)]",
                vec![("x_(1,2)", "R"), ("x_{1,2}", "IndexedVariable")],
            ),
            (
                "QQ[x_\"a\"]",
                vec![("x_\"a\"", "R"), ("x_\"b\"", "IndexedVariable")],
            ),
            (
                "QQ[x_1..z_1000000000]",
                vec![
                    ("x_1", "R"),
                    ("y_500000000", "R"),
                    ("z_1000000000", "R"),
                    ("z_1000000001", "IndexedVariable"),
                ],
            ),
        ] {
            assert_types(&format!("R = {constructor}"), &probes);
        }
    }

    /// Macaulay2 v1.26.05: reading `x_2` leaves `x` a `Symbol`; writing an
    /// index, by assignment or a ring constructor, makes `x` a table whose
    /// indices read as their latest write and whose other indices are bare
    /// `IndexedVariable`s.
    #[test]
    fn indexed_writes_turn_the_base_into_a_table_of_per_index_types() {
        assert_types("a = x_2", &[("x", "Symbol"), ("x_2", "IndexedVariable")]);
        assert_types(
            "x_2 = 4",
            &[
                ("x", "IndexedVariableTable"),
                ("x_2", "ZZ"),
                ("x_5", "IndexedVariable"),
            ],
        );
        assert_types(
            "x_2 = 4\nx_3 = \"three\"",
            &[("x_2", "ZZ"), ("x_3", "String")],
        );
        assert_types(
            "R = QQ[y_0..y_2]\ny_1 = 7",
            &[
                ("y", "IndexedVariableTable"),
                ("y_0", "R"),
                ("y_1", "ZZ"),
                ("y_9", "IndexedVariable"),
            ],
        );
        assert_types("x_2 = 4\nx = 3", &[("x", "ZZ")]);
    }

    #[test]
    fn polynomial_cartesian_ranges_preserve_holes_and_dimension_boundaries() {
        assert_types(
            "R = QQ[x_{1,2}..z_{2,4}, x_{4,1}, y_7, a..c, w]",
            &[
                ("x_{1,2}", "R"),
                ("y_{2,3}", "R"),
                ("z_{2,4}", "R"),
                ("x_{0,3}", "IndexedVariable"),
                ("x_{1,5}", "IndexedVariable"),
                ("x_{1}", "IndexedVariable"),
                ("x_{1,2,3}", "IndexedVariable"),
                ("x_{4,1}", "R"),
                ("y_{4,1}", "IndexedVariable"),
                ("y_7", "R"),
                ("x_7", "IndexedVariable"),
                ("w", "R"),
                ("b", "R"),
            ],
        );
    }
}
