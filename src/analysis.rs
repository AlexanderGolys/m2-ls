//! Staged analysis of one immutable document snapshot.
//!
//! The syntax-only scope pass establishes binding targets before enrichment
//! resolves their values, callable identities, and method installations. A
//! binding retains successive source-ordered states rather than one mutable
//! value, so queries before and after a reassignment can produce different facts.
//!
//! [`Analysis`] owns the resulting registry. [`TypeChecker`] performs transient
//! inference against those facts and the object knowledge visible at a requested
//! position. Editor features consume retained source ranges and semantic roles
//! instead of mutating analysis during a request.

use crate::node_metadata::SyntaxSnapshot;
mod control_flow;
mod enrichment;
mod first_pass;
mod installation;

pub use control_flow::{ControlTransfer, ControlTransferTarget, LoopStatement};
pub use inference::syntax::InfixSyntax;
pub(crate) use installation::MethodDeclarationOption;
mod inference;
mod known_value;
mod ring;
mod semantic_tokens;
mod syntax_roles;
mod typechecker;

pub use ring::RingVariableError;

use m2_syn::nodes::{
    Expr, ExprAssign, ExprOpBinAdj, ExprOpBinRegular as BinaryExpr, ExprOption as OptionExpression,
    ExprQuote as QuoteExpression,
};
use m2_syn::Token;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::ops::Deref;
use syntax_roles::{PropertyAccess, QuotedName};
use tower_lsp::lsp_types::{Position, Range as TextRange, SymbolKind};

use crate::builtin_index::{CallableKind, CallableOption, MethodSignature, OptionPassing, Record};
use crate::file_load::FileLoad;
use crate::meta::{BindingRole, Meta, Metadata};
use crate::node_metadata::matches_token;

use crate::object_registry::ObjectName;
use crate::object_registry::{ObjectId, ObjectKnowledge, OperatorForm, TypeData, TypeId};
use crate::package_index::PackageLoadingOption;
use crate::semantic_token::{SourceSemanticRole, SourceSemanticToken};
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::string_literal::LiteralSource;
use crate::syntax::ExpressionSyntax;
use crate::typesystem::{
    InferredType, LiteralOption, PositionedTypeKnowledge, SubtypeEvidence, Type, TypeKnowledge,
    TypeRole,
};
use crate::util::TextRangeExt;
use first_pass::{walk, BindingEffect, BindingFact, ScopeTree};
pub use inference::session::InferenceSession;
pub(crate) use typechecker::TypeChecker;

/// Identity of one scoped symbol binding within an immutable analysis snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingId(u32);

/// Identity of one source-ordered state of a scoped binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingStateId {
    /// The scoped binding to which these facts apply.
    binding: BindingId,
    /// Source-ordered state of the selected binding.
    state: usize,
}

/// An M2 operator — including `SPACE`, the juxtaposition operator (`X Y` is
/// `X SPACE Y`). Just another operator, not a special "adjacency" concept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Operator {
    /// Operator or semantic token represented by this item.
    pub token: ObjectName,
    /// Prefix, infix, or postfix form of the operator.
    pub form: OperatorForm,
}

impl Operator {
    /// Whether this operator's ordinary result is Boolean even without a catalog signature.
    pub fn is_comparison(&self) -> bool {
        [
            <Token![==] as Token>::SPELLING,
            <Token![===] as Token>::SPELLING,
            <Token![!=] as Token>::SPELLING,
            <Token![=!=] as Token>::SPELLING,
            <Token![<] as Token>::SPELLING,
            <Token![<=] as Token>::SPELLING,
            <Token![>] as Token>::SPELLING,
            <Token![>=] as Token>::SPELLING,
        ]
        .contains(&self.token.name())
    }

    /// Whether this binary operator is =, :=, or <- rather than an ordinary computation.
    fn is_assignment(&self) -> bool {
        self.form == OperatorForm::Binary
            && (matches_token::<Token![=]>(self.token.name())
                || matches_token::<Token![:=]>(self.token.name())
                || matches_token::<Token![<-]>(self.token.name()))
    }

    /// Whether this binary operator is the option-assignment arrow =>.
    fn is_option_assignment(&self) -> bool {
        self.form == OperatorForm::Binary && matches_token::<Token![=>]>(self.token.name())
    }
}

/// The callable or operator receiving an installed method.
/// Installation syntax affects the installed function's arity, not the
/// identity of this head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodHead {
    /// An ordinary callable name or its source expression.
    Function(ObjectName),
    /// An operator callable or its syntax classification.
    Operator(Operator),
}

impl MethodHead {
    /// Name used to identify this value in source and catalog lookups.
    pub fn name(&self) -> &ObjectName {
        match self {
            MethodHead::Function(name) => name,
            MethodHead::Operator(operator) => &operator.token,
        }
    }
}

/// Identity of one method-installing assignment within an analysis snapshot.
///
/// The identity indexes [`SemanticRegistry::installations`], allowing callable
/// records to refer directly to the source fact without a second method index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MethodInstallationId(usize);

/// Identity of one statically known callable runtime object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallableObjectId(usize);

/// Source-characterized method signature.
///
/// Domain and codomain names remain nominal here because local bindings and
/// package inclusions make their resolution source-position dependent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Callable or operator on which the method is installed.
    pub head: MethodHead,
    /// Argument identities accepted by the method signature.
    pub domain: Vec<ObjectName>,
    /// Result type of the method, when known.
    pub codomain: Option<ObjectName>,
}

impl Method {
    /// Combines a callable head, argument domain, and optional result type into a method signature.
    fn new(head: MethodHead, domain: Vec<ObjectName>, codomain: Option<ObjectName>) -> Self {
        Self {
            head,
            domain,
            codomain,
        }
    }
}

/// A function's argument shape, read from its lambda parameter node — the arity
/// of its domain, independent of any installed methods (a function with no
/// methods can still be total).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// Accepts a variable number of arguments.
    Variadic,
    /// Accepts exactly the recorded number of arguments.
    Fixed(usize),
}

/// A characterized assignment that installs one named [`Method`].
///
/// It is produced once during analysis and consumed by every capability instead
/// of each re-deciding installation syntax. `span` covers the whole assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodInstallation {
    /// Stable identity used to refer to this retained item.
    pub id: MethodInstallationId,
    /// Signature and callable information for the installation.
    pub method: Method,
    /// Source coordinates associated with this item.
    pub span: TextRange,
    /// Number of arguments required by this installation head.
    expected_argument_arity: usize,
    /// How the head passes options, which decides the form the installed
    /// function must have and which of its lambdas receives the arguments.
    pub options: MethodOptions,
    /// The form of the installed function.
    pub installed: InstalledValue,
    /// The shape of the installed function when it is a lambda, which
    /// receives the option table for [`MethodOptions::Table`].
    pub installed_lambda_dispatch: Option<Dispatch>,
    /// The shape of the lambda receiving the method's arguments, when the
    /// installed function has the form [`Self::options`] expect.
    pub argument_lambda_dispatch: Option<Dispatch>,
    /// Parsed syntax backing this item, when available.
    pub(crate) syntax: MethodInstallationSyntax,
    /// Whether the installation contributes a usable method.
    effect: MethodInstallationEffect,
    /// Where [`Method::codomain`] comes from; `Some` exactly when it is.
    pub codomain_origin: Option<CodomainOrigin>,
}

/// Where the codomain of a [`MethodInstallation`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodomainOrigin {
    /// Its annotation, the `T` of `f ZZ := T => x -> …`.
    Annotation,
    /// The `TypicalValue` of the method function it installs on.
    TypicalValue,
    /// Inference: the type the installed function always returns. Calls of
    /// the method return it as if it were annotated, and the
    /// `install-codomain-missing` hint suggests writing it down.
    Deduction,
}

/// How a method function passes options to the functions installed on it,
/// decided by the `Options` it was created with, independently of its runtime
/// class.
///
/// For `f ZZ := …`, a call `f 1` evaluates as follows:
///
/// | `method(…)` | installed value | `f 1` |
/// | --- | --- | --- |
/// | no `Options`, `Options => null` | `x -> body` | `body` |
/// | `Options => {…}`, `Options => false` | `o -> x -> body` | `body`, `o` the option table (any names for `false`) |
/// | `Options => true` | `{…} >> o -> x -> body` | `body`, `o` the declared options |
///
/// Any other form either fails at the call or yields something else, for
/// example `o -> x -> body` without options makes `f 1` a `FunctionClosure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodOptions {
    /// No options: installed functions receive the arguments.
    Absent,
    /// `Options => {…}`: installed functions receive the option table, merged
    /// from the declared defaults and the call, and return the function of the
    /// arguments.
    Table,
    /// `Options => true`: the call's options are forwarded with the arguments,
    /// so installed functions declare their own with `{…} >> o -> …`.
    Forwarded,
    /// A library `MethodFunctionWithOptions`: [`Self::Table`] or
    /// [`Self::Forwarded`], which the catalog does not record.
    TableOrForwarded,
    /// Neither the source nor the catalog tells whether the method has options.
    Unrecorded,
}

impl From<OptionPassing> for MethodOptions {
    fn from(passing: OptionPassing) -> Self {
        match passing {
            OptionPassing::Absent => Self::Absent,
            OptionPassing::Table | OptionPassing::Open => Self::Table,
            OptionPassing::Forwarded => Self::Forwarded,
        }
    }
}

/// A conflict between a supplied call option and the callable's accepted options.
///
/// Type checking produces these semantic facts for diagnostics and code actions.
/// Consumers choose diagnostic rules, messages, and source edits from the retained
/// option names, value types, and source nodes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CallOptionIssue<'ast> {
    /// A name none of the function's options has.
    UnknownName {
        /// Option or member key written by the source.
        key: &'ast Expr,
        /// The accepted name closest to the key, when one is close enough
        /// to be a misspelling of it.
        suggestion: Option<ObjectName>,
        /// Names or types the callable accepts in this position.
        accepted: Vec<ObjectName>,
    },
    /// A value whose type is none of the option's accepted types.
    ValueType {
        /// Value represented by this expression or semantic fact.
        value: &'ast Expr,
        /// Name of the callable option being checked.
        option: ObjectName,
        /// Inferred type of the supplied value.
        actual: InferredType,
        /// Names or types the callable accepts in this position.
        accepted: Vec<ObjectName>,
    },
    /// A symbol outside the values an enumeration-like option accepts.
    ValueNotPossible {
        /// Value represented by this expression or semantic fact.
        value: &'ast Expr,
        /// Name of the callable option being checked.
        option: ObjectName,
        /// Candidates remaining compatible with the available evidence.
        possible: Vec<ObjectName>,
    },
}

impl MethodOptions {
    /// The options implied by a method function's runtime class, or `None`
    /// when `class` is not a method function class. `MethodFunctionSingle` and
    /// `MethodFunctionBinary` exist with and without options.
    pub fn of_runtime_class(
        class: &ObjectName,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Self> {
        if knowledge.has_type_role(class, TypeRole::MethodFunctionWithOptions) {
            Some(Self::TableOrForwarded)
        } else if knowledge.has_type_role(class, TypeRole::MethodFunction) {
            Some(Self::Absent)
        } else if knowledge.has_type_role(class, TypeRole::MethodFunctionSingle)
            || knowledge.has_type_role(class, TypeRole::MethodFunctionBinary)
        {
            Some(Self::Unrecorded)
        } else {
            None
        }
    }
}

/// A list declaring the options a function takes, with their defaults. Only
/// these lists name options; the same `key => value` entry anywhere else is a
/// hash table entry or an option a call passes to some function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptionDeclaration {
    /// The list of `method(Options => {…})`, the options of every
    /// installation of the method.
    Method,
    /// The list of `{…} >> o -> …`, the options of one function.
    Function,
}

/// The form of an installation's stored function that decides whether it
/// fits the head's [`MethodOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledValue {
    /// A lambda: `x -> …`, or `o -> x -> …` receiving an option table.
    Lambda,
    /// `{…} >> o -> …`, a function declaring its own options.
    OptionDeclaring,
    /// A library function, which neither receives an option table first nor
    /// returns a function of the arguments.
    LibraryFunction {
        /// Whether it accepts options; assumed for every method function
        /// class that may have them, and for records listing options.
        declares_options: bool,
    },
    /// A value the analysis does not characterize, such as a local name.
    Unknown,
}

/// The source construct that installs a method implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MethodInstallationSyntax {
    /// An installation written using the assignment syntax.
    Classical,
    /// An installation made through an installMethod call.
    InstallMethod,
}

/// Whether an installation is rejected, unresolved, or effective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MethodInstallationEffect {
    /// The installation is invalid and contributes no callable method.
    Rejected,
    /// The installation cannot yet be associated with a known callable.
    Unresolved,
    /// The installation contributes a usable callable method.
    Effective,
}

/// A lambda-body codomain that can safely drive an annotation quick fix.
pub struct MethodCodomainDeduction {
    /// Result type of the method, when known.
    pub codomain: ObjectName,
    /// Explicit result-type annotation, when the source supplies one.
    pub annotated_codomain: Option<ObjectName>,
    /// Source extent to highlight for the reported finding.
    pub diagnostic_range: TextRange,
    /// Replacement policy used when updating the result-type annotation.
    pub edit: MethodCodomainEdit,
}

/// The source edit needed to make a method codomain annotation precise.
pub enum MethodCodomainEdit {
    /// Inserts the missing annotation at the recorded source extent.
    Add(TextRange),
    /// Replaces an existing annotation in place.
    Replace,
}

/// The outline-relevant semantic shape of one characterized assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentFactKind {
    /// Assignment introducing a method installation.
    MethodInstallation(MethodInstallationId),
    /// Assignment to an indexed variable-table entry.
    IndexedVariable,
    /// Callable resolved through a source binding.
    ScopedCallable,
}

/// Source-facing assignment facts retained by analysis for capability projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentFact {
    /// Text presented to the editor or user.
    pub label: String,
    /// Source coordinates associated with this item.
    pub span: TextRange,
    /// Source extent of the assignment target.
    pub target_span: TextRange,
    /// Source extent of the assigned expression, when one exists.
    pub value_span: Option<TextRange>,
    /// Index of the lexical scope owning this operation.
    pub scope_idx: usize,
    /// Semantic or presentation category of this item.
    pub kind: AssignmentFactKind,
}

impl MethodInstallation {
    /// The argument count the lambda receiving the method's arguments must take.
    pub fn expected_argument_arity(&self) -> usize {
        self.expected_argument_arity
    }

    /// Whether this value takes effect.
    pub fn takes_effect(&self) -> bool {
        self.effect == MethodInstallationEffect::Effective
    }

    /// Whether this installation is effective and can be exposed to cross-file method lookup.
    pub fn is_workspace_candidate(&self) -> bool {
        self.effect != MethodInstallationEffect::Rejected
    }
}

/// Syntax-derived callable behavior used to decide whether method
/// installations take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalFunctionKind {
    /// Ordinary callable without method-option forwarding.
    Plain,
    /// Method callable carrying its option-passing policy.
    Method(MethodOptions),
}

/// How a callable head receives arguments and options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallableHeadKind {
    /// Ordinary function without method dispatch.
    PlainFunction,
    /// Method-dispatching function with its recorded option policy.
    MethodFunction(MethodOptions),
    /// No narrower static result can be established.
    Unknown,
}

/// Semantic information about one statically tracked callable.
///
/// Installed methods are referenced by identity in the snapshot's semantic
/// registry
/// so their source facts have one owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionInfo {
    /// Known representative result type for this callable.
    pub typical_value: Option<ObjectName>,
    /// Method installations associated with this callable or analysis.
    pub installations: Vec<MethodInstallationId>,
    /// Fixed or variadic argument shape known for the function.
    pub dispatch: Option<Dispatch>,
    /// Recognized runtime class controlling callable behavior.
    runtime_class: Option<TypeRole>,
    /// Catalog callable whose behavior this source value follows.
    indexed_callable: Option<ObjectName>,
    /// Semantic or presentation category of this item.
    kind: LocalFunctionKind,
    /// The options its source declares, with `method(Options => {…})` or
    /// `{…} >> o -> …`; `None` when it declares none or they are not a
    /// literal list.
    pub declared_options: Option<Vec<CallableOption>>,
}

impl FunctionInfo {
    /// Whether this node is a method function.
    pub fn is_method_function(&self) -> bool {
        matches!(self.kind, LocalFunctionKind::Method(_))
    }
}

/// Static facts computed for one call after separating positional arguments
/// from literal option assignments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallStaticFacts {
    /// Inferred types of the arguments supplied at the call site.
    pub argument_types: Vec<InferredType>,
    /// Option assignments whose keys and values can be recognized statically.
    pub literal_options: Vec<LiteralOption>,
}

/// Installed signatures classified by the typechecker's dispatch-range result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallSignatureFacts {
    /// The single applicable signature, when selection is unambiguous.
    pub pinned: Option<MethodSignature>,
    /// Candidates remaining compatible with the available evidence.
    pub possible: Vec<MethodSignature>,
    /// Signatures ruled out by the known argument types.
    pub excluded: Vec<MethodSignature>,
}

/// Source-independent identity and editor-facing anchor of one lexical binding.
///
/// Its value and inferred type at a particular point live in
/// [`BindingStateInfo`] records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingInfo {
    /// Stable identity of the scoped source binding.
    pub binding_id: BindingId,
    /// Name used to resolve and present this item.
    pub name: ObjectName,
    /// How the name participates in the binding or semantic operation.
    pub role: BindingRole,
    /// Whether this binding can become visible outside the source file.
    pub potential_export: bool,
    /// Source extent used to locate this item.
    pub range: TextRange,
    /// Index of the lexical scope owning this operation.
    pub scope_idx: usize,
    /// Successive values of the binding, ordered by their source position.
    pub states: Vec<BindingStateInfo>,
    /// Whether an assignment creates this binding rather than updating an existing one.
    introduced_by_assignment: bool,
}

impl BindingInfo {
    /// Whether the binding is a local of a file, function or block: a parameter,
    /// or a name declared with `:=`. A name assigned with `=`
    /// inside a function, with no such local in scope, is the global.
    pub fn is_local(&self) -> bool {
        self.role == BindingRole::Parameter || !self.introduced_by_assignment
    }
}

/// One source-ordered value and inferred-type state of a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingStateInfo {
    /// Symbol category used for editor presentation.
    pub presentation_kind: SymbolKind,
    /// Type inferred from the binding's current value, when known.
    pub inferred_type: Option<InferredType>,
    /// Identity of the callable object associated with this value.
    pub object_id: Option<CallableObjectId>,
    /// Known types of entries in an indexed variable table.
    pub indexed_element_type: Option<ring::IndexedVariableTypes>,
    /// The value the state certainly holds, where the program determines
    /// more of it than its type: the ring `QQ[x, y]` of `S = QQ[x, y]`.
    pub known_value: Option<known_value::KnownValue>,
    /// Whether this value is a generator introduced by a ring constructor.
    pub is_ring_generator: bool,
    /// Canonical value identity when construction or aliasing establishes one.
    pub value_identity: Option<ObjectId>,
    /// Identity of a runtime class declared by the source.
    pub source_type: Option<TypeId>,
    /// Source extent of the expression supplying the binding's value.
    pub value_range: Option<TextRange>,
    /// Enclosing declaration extent shown as the definition site.
    pub definition_range: TextRange,
    /// Source coordinates associated with this item.
    pub span: TextRange,
    /// Index of the lexical scope owning this operation.
    pub scope_idx: usize,
}

impl BindingStateInfo {
    /// Position from which the recorded binding state is visible to later source.
    fn effective_from(&self) -> Position {
        self.value_range.map_or(self.span.end, |value| value.end)
    }
}

/// A binding declaration paired with the state effective at a query position.
#[derive(Debug, Clone, Copy)]
pub struct BindingView<'a> {
    /// The scoped binding to which these facts apply.
    pub binding: &'a BindingInfo,
    /// Source-ordered state of the selected binding.
    pub state: &'a BindingStateInfo,
}

impl Deref for BindingView<'_> {
    type Target = BindingInfo;

    fn deref(&self) -> &Self::Target {
        self.binding
    }
}

impl Metadata for BindingView<'_> {
    fn meta(&self) -> Meta {
        Meta {
            symbol_kind: Some(self.state.presentation_kind),
            binding_role: Some(self.role),
            type_label: self
                .state
                .inferred_type
                .as_ref()
                .and_then(InferredType::label),
        }
    }
}

/// Typed source-declared type edges and their source-symbol identities.
#[derive(Debug)]
struct SourceTypeFacts {
    /// Stored semantic data indexed by the corresponding identity.
    data: HashMap<TypeId, TypeData>,
    /// Distinguishes independently analyzed documents and embedded sessions.
    namespace: u64,
    /// Canonical runtime classes of source-created objects and type objects.
    classes: HashMap<ObjectId, TypeId>,
}

impl Default for SourceTypeFacts {
    fn default() -> Self {
        Self {
            data: HashMap::new(),
            namespace: NEXT_SOURCE_NAMESPACE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            classes: HashMap::new(),
        }
    }
}

/// Monotonic identities prevent source objects in different snapshots from colliding.
static NEXT_SOURCE_NAMESPACE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Start and end UTF-16 coordinates used as a compact source-range lookup key.
type SourceRangeKey = [u32; 4];

/// Encodes the start and end UTF-16 positions as a compact hash-map key.
fn source_range_key(range: TextRange) -> SourceRangeKey {
    [
        range.start.line,
        range.start.character,
        range.end.line,
        range.end.character,
    ]
}

/// Canonical per-snapshot store of symbols, bindings, scopes, and their indexes.
#[warn(missing_docs)]
#[derive(Debug, Default)]
struct SemanticRegistry {
    /// Lexical scopes collected in traversal order.
    scopes: ScopeTree,
    /// Bindings collected for the containing analysis or scope.
    bindings: Vec<BindingInfo>,
    /// Name-to-binding lookup retaining distinct scoped declarations.
    bindings_by_name: HashMap<ObjectName, Vec<BindingId>>,
    /// Source coordinates identifying the state written at an assignment.
    binding_states_by_range: HashMap<SourceRangeKey, BindingStateId>,
    /// Callable identities discovered while analyzing source values.
    callable_objects: Vec<FunctionInfo>,
    /// Catalog-to-local callable identity mapping.
    indexed_callable_objects: HashMap<ObjectId, CallableObjectId>,
    /// Methods grouped by operator spelling and form.
    operator_functions: HashMap<Operator, FunctionInfo>,
    /// Method installations associated with this callable or analysis.
    installations: Vec<MethodInstallation>,
    /// Installations by the lambda receiving their arguments,
    /// whose parameters take the installation's domain types.
    installations_by_argument_lambda: HashMap<SourceRangeKey, MethodInstallationId>,
    /// Source-ordered assignments used by later editor features.
    assignment_facts: Vec<AssignmentFact>,
    /// Roles collected before final token classification.
    pending_source_semantic_roles: Vec<(TextRange, SourceSemanticRole)>,
    /// Classified source spans retained for editor responses.
    source_semantic_tokens: Vec<SourceSemanticToken>,
    /// Runtime classes and ancestry introduced by this source file.
    source_types: SourceTypeFacts,
    /// Generator bindings grouped by the ring that introduced them.
    ring_generators: HashMap<ObjectName, Vec<ring::RingGenerator>>,
    /// Where ring generators are declared, in the order their bindings were
    /// made: each bound constructor's variable list and `Variable` option.
    ring_generator_specs: Vec<TextRange>,
}

#[derive(Clone, Copy)]
pub enum OutputReference {
    /// Output reference counted backward from the current source cell.
    Relative(NonZeroUsize),
    /// Output reference selecting the recorded absolute output number.
    Absolute(NonZeroUsize),
    /// Absolute output reference whose target is not available in the document.
    MissingAbsolute,
}

impl OutputReference {
    /// Parses the supplied source or name into this representation.
    pub fn parse(name: &str) -> Option<Self> {
        let relative_distance = match name {
            "oo" => Some(1),
            "ooo" => Some(2),
            "oooo" => Some(3),
            _ => None,
        };
        if let Some(distance) = relative_distance.and_then(NonZeroUsize::new) {
            return Some(Self::Relative(distance));
        }
        let number = name.strip_prefix('o')?;
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

#[derive(Debug, Clone)]
/// Type and presentation facts added after a binding has been registered.
#[warn(missing_docs)]
struct BindingEnrichment {
    /// Symbol category used for editor presentation.
    presentation_kind: SymbolKind,
    /// Type inferred from the binding's current value, when known.
    inferred_type: Option<InferredType>,
    /// Identity of the callable object associated with this value.
    object_id: Option<CallableObjectId>,
    /// Known types of entries in an indexed variable table.
    indexed_element_type: Option<ring::IndexedVariableTypes>,
    /// Resolved superclass of a source-declared runtime class.
    parent_type: Option<TypeId>,
}

/// Complete semantic analysis of one immutable document snapshot.
/// It owns source facts and characterized method installations; expression
/// types are not retained after inference.
#[derive(Debug)]
pub struct Analysis {
    /// Object and binding registry used to resolve semantic facts.
    registry: SemanticRegistry,
}

impl Analysis {
    /// The binding a mention of `name` at `pos` — a name in a comment or a
    /// string, not code — refers to: the one in scope there, or else a global
    /// of that name, which a mention may name before its definition. A local
    /// exists only in the scope declaring it, so no mention outside refers to
    /// it.
    pub fn mention_symbol_at(&self, name: &str, pos: Position) -> Option<BindingView<'_>> {
        self.get_binding_at(name, pos).or_else(|| {
            self.registry
                .bindings_by_name
                .get(name)?
                .iter()
                .filter_map(|binding_id| self.binding_anchor(*binding_id))
                .find(|binding| binding.scope_idx == 0)
        })
    }

    /// Visible binding state selected by name at the requested source position.
    pub fn get_binding_at(&self, name: &str, pos: Position) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(pos)?;
        self.get_binding_from_scope(name, scope_idx, pos)
    }

    /// The binding actually written by a declaration or assignment target.
    /// Uses the retained target identity so later declarations in an enclosing
    /// scope cannot change which symbol an earlier assignment writes.
    pub fn assigned_binding_at(&self, target: TextRange) -> Option<BindingView<'_>> {
        let id = self
            .registry
            .binding_states_by_range
            .get(&source_range_key(target))?;
        let binding = self.binding(id.binding)?;
        let state = binding.states.get(id.state)?;
        Some(BindingView { binding, state })
    }

    /// Source binding at available under the current source position and import context.
    pub fn visible_source_binding_at(
        &self,
        name: &str,
        pos: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(pos)?;
        self.visible_source_binding_from_scope(name, scope_idx, pos, knowledge)
    }

    /// Source binding from scope available under the current source position and import context.
    fn visible_source_binding_from_scope(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<BindingView<'_>> {
        self.get_binding_from_scope(name, scope_idx, pos)
            .filter(|binding| Self::source_binding_is_visible(*binding, knowledge))
    }

    /// Whether the binding is visible under the selected source position and primary-code policy.
    pub fn source_binding_is_visible(
        binding: BindingView<'_>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        binding.scope_idx != 0 || !knowledge.shadows_source(&binding.name, binding.state.span.start)
    }

    /// Finds the visible state of a named binding, searching outward from the supplied scope.
    fn get_binding_from_scope(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
    ) -> Option<BindingView<'_>> {
        let binding_id = self.binding_id_from_scope(name, scope_idx, pos)?;
        self.binding_state_from_scope(binding_id, scope_idx, pos)
    }

    /// Identity of the named binding visible while searching outward from the supplied lexical scope.
    fn binding_id_from_scope(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
    ) -> Option<BindingId> {
        self.binding_id_from_scope_in(name, scope_idx, pos, &self.registry.scopes)
    }

    /// Resolves a name against the supplied binding collection starting in the selected lexical scope.
    fn binding_id_from_scope_in(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
        scopes: &ScopeTree,
    ) -> Option<BindingId> {
        let mut curr = Some(scope_idx);
        while let Some(idx) = curr {
            // In the use's own scope a binding governs only from its definition
            // onward: a use textually before a local `:=` sees the outer binding,
            // not the not-yet-declared local. Ancestor scopes are closures
            // evaluated at call time, after the file is fully read, so a forward
            // reference to an outer name defined later still resolves to it.
            let constrain_to_prior = idx == scope_idx;
            let binding_id = self
                .registry
                .bindings_by_name
                .get(name)
                .into_iter()
                .flatten()
                .filter_map(|binding_id| {
                    self.binding(*binding_id)
                        .map(|binding| (*binding_id, binding))
                })
                .filter(|binding| {
                    binding.1.scope_idx == idx
                        && (!constrain_to_prior || binding.1.range.start <= pos)
                })
                .max_by_key(|(_, binding)| {
                    (binding.range.start.line, binding.range.start.character)
                })
                .map(|(binding_id, _)| binding_id);
            if binding_id.is_some() {
                return binding_id;
            }
            curr = scopes.parent(idx);
        }
        None
    }

    /// The local that `name = v` written at `position` assigns: a `:=` local
    /// or parameter of an enclosing function, declared before the assignment.
    /// M2 resolves the name where the assignment is written, so such a local is
    /// assigned even from a nested branch or closure; a local declared later is
    /// not yet visible there, and without any local the assignment targets the
    /// global symbol.
    fn assigned_local_binding(
        &self,
        name: &str,
        position: Position,
        scopes: &ScopeTree,
    ) -> Option<BindingId> {
        let mut scope = scopes.at(position);
        while let Some(scope_idx) = scope.filter(|scope_idx| *scope_idx != 0) {
            let declared = self
                .registry
                .bindings_by_name
                .get(name)
                .into_iter()
                .flatten()
                .filter_map(|binding_id| self.binding(*binding_id))
                .filter(|binding| {
                    binding.scope_idx == scope_idx
                        && !binding.introduced_by_assignment
                        && binding.range.start <= position
                })
                .max_by_key(|binding| (binding.range.start.line, binding.range.start.character))
                .map(|binding| binding.binding_id);
            if declared.is_some() {
                return declared;
            }
            scope = scopes.parent(scope_idx);
        }
        None
    }

    /// Binding record addressed by the supplied stable identity.
    fn binding(&self, binding_id: BindingId) -> Option<&BindingInfo> {
        self.registry.bindings.get(binding_id.0 as usize)
    }

    /// Later assignment of this name used as an anchor before its first visible write.
    fn future_assignment_binding(
        &self,
        name: &str,
        scope_idx: usize,
        position: Position,
    ) -> Option<BindingId> {
        self.registry
            .bindings_by_name
            .get(name)?
            .iter()
            .filter_map(|binding_id| {
                self.binding(*binding_id)
                    .map(|binding| (*binding_id, binding))
            })
            .filter(|(_, binding)| {
                binding.scope_idx == scope_idx
                    && binding.introduced_by_assignment
                    && binding.range.start > position
            })
            .min_by_key(|(_, binding)| binding.range.start)
            .map(|(binding_id, _)| binding_id)
    }

    /// Later assignment that can supply the named binding's declaration anchor.
    pub fn future_assignment_binding_at(
        &self,
        name: &str,
        position: Position,
    ) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(position)?;
        let binding_id = self.future_assignment_binding(name, scope_idx, position)?;
        self.binding_anchor(binding_id)
    }

    /// Anchor associated with the selected source binding and its current state.
    fn binding_anchor(&self, binding_id: BindingId) -> Option<BindingView<'_>> {
        let binding = self.binding(binding_id)?;
        let state = binding.states.first()?;
        Some(BindingView { binding, state })
    }

    /// Latest visible value of the named binding while searching outward from the supplied lexical scope.
    fn binding_state_from_scope(
        &self,
        binding_id: BindingId,
        scope_idx: usize,
        pos: Position,
    ) -> Option<BindingView<'_>> {
        let binding = self.binding(binding_id)?;
        let mut curr = Some(scope_idx);
        while let Some(idx) = curr {
            let constrain_to_prior = idx == scope_idx;
            if let Some(state) = binding
                .states
                .iter()
                .find(|state| state.scope_idx == idx && state.span.contains_position(pos))
            {
                return Some(BindingView { binding, state });
            }
            let state = binding
                .states
                .iter()
                .enumerate()
                .filter(|state| {
                    state.1.scope_idx == idx
                        && (!constrain_to_prior || state.1.effective_from() <= pos)
                })
                .max_by_key(|(order, state)| (state.effective_from(), *order))
                .map(|(_, state)| state);
            if let Some(state) = state {
                return Some(BindingView { binding, state });
            }
            curr = self.registry.scopes.parent(idx);
        }
        self.binding_anchor(binding_id)
    }

    /// Callable facts attached to the named binding at a source position.
    pub fn function_at(&self, name: &str, position: Position) -> Option<&FunctionInfo> {
        let binding = self.get_binding_at(name, position)?;
        self.function_for_binding(binding)
    }

    /// Result class attached to a source installation, when it can be resolved.
    pub fn method_installation_codomain<'a>(
        &'a self,
        installation: &'a MethodInstallation,
    ) -> Option<&'a str> {
        installation.method.codomain.as_ref().map(ObjectName::name)
    }

    /// Callable facts associated with the selected binding state.
    pub fn function_for_binding(&self, binding: BindingView<'_>) -> Option<&FunctionInfo> {
        self.registry
            .callable_objects
            .get(binding.state.object_id?.0)
    }

    /// The installation each domain of `function` dispatches to, the last
    /// one installed on it, in installation order.
    pub fn effective_installations<'a>(
        &'a self,
        function: &'a FunctionInfo,
    ) -> impl Iterator<Item = &'a MethodInstallation> + 'a {
        function
            .installations
            .iter()
            .rev()
            .filter_map(|id| self.method_installation(*id))
            .fold(Vec::new(), |mut installations, installation| {
                if !installations.iter().any(|effective: &&MethodInstallation| {
                    effective.method.domain == installation.method.domain
                }) {
                    installations.push(installation);
                }
                installations
            })
            .into_iter()
            .rev()
    }

    /// Source installations for the selected callable that are effective at this source position.
    fn method_installations_for_at<'a>(
        &'a self,
        function: &FunctionInfo,
        position: Position,
    ) -> Vec<&'a MethodInstallation> {
        let mut installations = Vec::new();
        for installation in function
            .installations
            .iter()
            .rev()
            .filter_map(|id| self.method_installation(*id))
            .filter(|installation| installation.span.start <= position)
        {
            if !installations.iter().any(|installed: &&MethodInstallation| {
                installed.method.domain == installation.method.domain
            }) {
                installations.push(installation);
            }
        }
        installations.reverse();
        installations
    }

    /// Retained method installation with the supplied identity.
    pub fn method_installation(&self, id: MethodInstallationId) -> Option<&MethodInstallation> {
        self.registry.installations.get(id.0)
    }

    /// All retained method installations in source discovery order.
    pub fn installations(&self) -> &[MethodInstallation] {
        &self.registry.installations
    }

    /// Assignments retained for subsequent hints and semantic queries.
    pub fn assignment_facts(&self) -> &[AssignmentFact] {
        &self.registry.assignment_facts
    }

    /// Scope whose registered extent exactly matches the supplied range.
    pub fn scope_with_range(&self, range: TextRange) -> Option<usize> {
        self.registry.scopes.with_range(range)
    }

    /// Number of lexical scopes retained for this analysis.
    pub fn scope_count(&self) -> usize {
        self.registry.scopes.len()
    }

    /// Enclosing scope index, absent for the document scope.
    pub fn parent_scope(&self, scope_idx: usize) -> Option<usize> {
        self.registry.scopes.parent(scope_idx)
    }

    /// Syntax and semantic roles already classified for editor features.
    pub fn source_semantic_tokens(&self) -> &[SourceSemanticToken] {
        &self.registry.source_semantic_tokens
    }

    /// Binding views declared directly in the selected lexical scope.
    pub fn bindings_in_scope(&self, scope_idx: usize) -> impl Iterator<Item = BindingView<'_>> {
        self.bindings()
            .filter(move |binding| binding.scope_idx == scope_idx)
    }

    /// Binding views across all lexical scopes in this document.
    pub fn bindings(&self) -> impl Iterator<Item = BindingView<'_>> {
        self.registry
            .bindings
            .iter()
            .filter_map(|binding| self.binding_anchor(binding.binding_id))
    }

    /// All source-ordered values recorded for the document's bindings.
    pub fn binding_states(&self) -> impl Iterator<Item = BindingView<'_>> {
        self.registry.bindings.iter().flat_map(|binding| {
            binding
                .states
                .iter()
                .map(|state| BindingView { binding, state })
        })
    }

    /// Visible bindings whose names match the supplied prefix at a source position.
    pub fn in_scope_bindings(&self, prefix: &str, pos: Position) -> Vec<BindingView<'_>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut current = self.find_scope_at(pos);
        while let Some(idx) = current {
            for binding in self.bindings_in_scope(idx) {
                if binding.name.name().starts_with(prefix) && seen.insert(binding.name.clone()) {
                    out.push(binding);
                }
            }
            current = self.registry.scopes.parent(idx);
        }
        out
    }

    /// Innermost lexical scope containing the source position.
    pub(crate) fn find_scope_at(&self, pos: Position) -> Option<usize> {
        self.registry.scopes.at(pos)
    }

    /// Runtime lexical dictionaries with their extents and enclosing dictionary identities.
    pub fn scope_ranges(&self) -> impl Iterator<Item = (usize, TextRange, Option<usize>)> + '_ {
        self.registry.scopes.entries()
    }

    /// Runtime lexical dictionary containing a semantic analysis scope.
    pub fn dictionary_scope(&self, scope: usize) -> Option<usize> {
        self.registry.scopes.dictionary(scope)
    }

    /// Runs binding collection and semantic enrichment against positioned object knowledge.
    pub fn new_with_knowledge(
        syntax: &SyntaxSnapshot,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
    ) -> Self {
        Self::new_in_namespace(syntax, source, knowledge, None)
    }

    /// Identity namespace for declarations of this syntax revision. Context-only
    /// reanalysis retains it so importing files keep referring to the same objects.
    pub fn source_namespace(&self) -> u64 {
        self.registry.source_types.namespace
    }

    /// Analyzes a syntax revision using an existing declaration namespace, or
    /// allocates one for new syntax when `namespace` is absent.
    pub fn new_in_namespace(
        syntax: &SyntaxSnapshot,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
        namespace: Option<u64>,
    ) -> Self {
        let mut analysis = Analysis {
            registry: SemanticRegistry::default(),
        };
        if let Some(namespace) = namespace {
            analysis.registry.source_types.namespace = namespace;
        }
        let source = crate::source::SyntaxNavigation::new(source, syntax);
        let bindings_timing = crate::telemetry::Timing::start(
            "analysis.bindings",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.registry.scopes = walk(syntax, &source, |fact, scopes| {
            analysis.collect_binding_fact(fact, scopes);
        });
        drop(bindings_timing);
        let inference_timing = crate::telemetry::Timing::start(
            "analysis.inference",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.enrich_types(&source, knowledge);
        analysis.retain_object_classes(knowledge);
        drop(inference_timing);
        let _semantics_timing = crate::telemetry::Timing::start(
            "analysis.semantic_tokens",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.collect_semantics(&source, knowledge);
        analysis
    }

    /// Canonical identity of a value assigned to a source binding in this snapshot.
    pub fn source_object_id(&self, binding: BindingId, state: usize) -> ObjectId {
        ObjectId::new(format!(
            "$Source${}:{}:{state}",
            self.registry.source_types.namespace, binding.0
        ))
    }

    /// The object represented by a source binding state, independent of its kind.
    pub fn binding_object_id(&self, binding: BindingView<'_>) -> ObjectId {
        if let Some(identity) = &binding.state.value_identity {
            return identity.clone();
        }
        let state = binding
            .binding
            .states
            .iter()
            .position(|state| std::ptr::eq(state, binding.state))
            .expect("binding views reference a retained state");
        self.source_object_id(binding.binding_id, state)
    }

    /// Resolves class edges once at each assignment rather than through later names.
    fn retain_object_classes(&mut self, knowledge: &(impl PositionedTypeKnowledge + ?Sized)) {
        let classes = self
            .binding_states()
            .filter_map(|binding| {
                let class = binding.state.inferred_type.as_ref()?.single()?;
                let position = binding
                    .state
                    .value_range
                    .map_or(binding.state.span.start, |value| value.start);
                let positioned = knowledge.at_position(position);
                let object = self.binding_object_id(binding);
                let class = binding
                    .state
                    .inferred_type
                    .as_ref()
                    .and_then(InferredType::single_identity)
                    .cloned()
                    .or_else(|| positioned.class_type_id(&object))
                    .or_else(|| {
                        TypeChecker::new(self, knowledge).resolve_type_id_at(
                            class,
                            position,
                            &positioned,
                        )
                    })?;
                Some((object, class))
            })
            .collect::<Vec<_>>();
        self.registry.source_types.classes.extend(classes);
    }

    /// Parent and class relations queried identically for source and catalog objects.
    pub fn object_relations<'a, K: ObjectKnowledge + ?Sized>(
        &'a self,
        knowledge: &'a K,
    ) -> crate::object_hierarchy::ObjectRelations<'a, K> {
        crate::object_hierarchy::ObjectRelations::new(
            &self.registry.source_types.data,
            &self.registry.source_types.classes,
            knowledge,
        )
    }

    /// Incorporates one syntax-pass binding fact into semantic state.
    fn collect_binding_fact(&mut self, fact: BindingFact, scopes: &ScopeTree) {
        self.collect_definition(fact, scopes);
    }

    /// Whether `name` denotes a TYPE — the hinge of the installation rules. The
    /// type universe is layered:
    /// a local binding whose inferred class is `Type` (e.g. `X = new Type of …`)
    /// takes precedence over objects visible in the current environment.
    fn operand_is_type(
        &self,
        name: &str,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        self.get_binding_at(name, position)
            .is_some_and(|binding| binding.state.source_type.is_some())
            || knowledge
                .get_record(&ObjectName::new(name))
                .is_some_and(|record| record.type_info().is_some())
    }

    /// Whether a source callable head denotes a plain function or method dispatcher.
    pub(crate) fn callable_head_kind(
        &self,
        name: &str,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallableHeadKind {
        if let Some(binding) = self.get_binding_at(name, position) {
            if let Some(function) = self.function_for_binding(binding) {
                return match function.kind {
                    LocalFunctionKind::Plain => CallableHeadKind::PlainFunction,
                    LocalFunctionKind::Method(options) => CallableHeadKind::MethodFunction(options),
                };
            }
            return binding
                .state
                .inferred_type
                .as_ref()
                .and_then(InferredType::single)
                .map_or(
                    CallableHeadKind::Unknown,
                    |type_name| match MethodOptions::of_runtime_class(type_name, knowledge) {
                        Some(options) => CallableHeadKind::MethodFunction(options),
                        None if knowledge.has_type_role(type_name, TypeRole::Function) => {
                            CallableHeadKind::PlainFunction
                        }
                        None => CallableHeadKind::Unknown,
                    },
                );
        }
        knowledge
            .get_record(&ObjectName::new(name))
            .and_then(|record| Some((record, record.callable()?)))
            .map_or(CallableHeadKind::Unknown, |(record, callable)| {
                if callable.is_method_function() {
                    CallableHeadKind::MethodFunction(Self::catalog_method_options(
                        record, knowledge,
                    ))
                } else {
                    CallableHeadKind::PlainFunction
                }
            })
    }

    /// The options of a catalog method function, as the catalog records
    /// them, or else as its runtime class implies. A record without either
    /// leaves them unrecorded.
    fn catalog_method_options(
        record: &Record,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> MethodOptions {
        record
            .callable()
            .and_then(|callable| callable.option_passing)
            .map(MethodOptions::from)
            .or_else(|| MethodOptions::of_runtime_class(&record.class, knowledge))
            .unwrap_or(MethodOptions::Unrecorded)
    }

    /// Collects definition from the supplied syntax and semantic context.
    fn collect_definition(&mut self, fact: BindingFact, scopes: &ScopeTree) {
        let name = fact.name.name();
        match fact.effect {
            BindingEffect::Declare => {
                self.add_binding(fact);
            }
            BindingEffect::Assign => {
                let position = fact.target.start;
                let binding_id =
                    self.assigned_local_binding(name, position, scopes)
                        .or_else(|| {
                            self.binding_id_from_scope_in(name, fact.scope, position, scopes)
                                .filter(|binding_id| {
                                    self.binding_anchor(*binding_id)
                                        .is_some_and(|binding| binding.scope_idx == fact.scope)
                                })
                        });
                if let Some(binding_id) = binding_id {
                    self.add_binding_state(binding_id, fact);
                } else {
                    self.add_binding(fact);
                }
            }
        }
    }

    /// Enriches binding state using the bindings and type knowledge already collected.
    fn enrich_binding_state(
        &mut self,
        binding_id: BindingId,
        state_index: usize,
        enrichment: BindingEnrichment,
    ) {
        let source_type = enrichment.parent_type.map(|parent_type| {
            let type_id = TypeId::from_source_type(
                self.source_object_id(binding_id, state_index),
                &parent_type,
            );
            self.registry.source_types.data.insert(
                type_id.clone(),
                TypeData {
                    parent: Some(parent_type),
                },
            );
            type_id
        });
        let Some(state) = self
            .registry
            .bindings
            .get_mut(binding_id.0 as usize)
            .and_then(|binding| binding.states.get_mut(state_index))
        else {
            return;
        };
        state.presentation_kind = enrichment.presentation_kind;
        state.inferred_type = enrichment.inferred_type;
        state.object_id = enrichment.object_id;
        state.indexed_element_type = enrichment.indexed_element_type;
        state.value_identity = source_type.as_ref().map(|type_id| type_id.object().clone());
        state.source_type = source_type;
    }

    /// Registers a new scoped name and returns its stable binding identity.
    fn add_binding(&mut self, fact: BindingFact) -> BindingId {
        let name = fact.name.clone();
        let binding_id = BindingId(self.registry.bindings.len() as u32);
        let range = fact.target;
        let state = BindingStateInfo {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: None,
            object_id: None,
            indexed_element_type: None,
            known_value: None,
            is_ring_generator: false,
            source_type: None,
            value_identity: None,
            value_range: fact.value,
            definition_range: fact.definition,
            span: range,
            scope_idx: fact.scope,
        };
        let binding = BindingInfo {
            binding_id,
            name: name.clone(),
            role: fact.role,
            potential_export: fact.potential_export,
            range,
            scope_idx: fact.scope,
            states: vec![state],
            introduced_by_assignment: matches!(fact.effect, BindingEffect::Assign),
        };
        self.registry.bindings.push(binding);
        self.registry.binding_states_by_range.insert(
            source_range_key(fact.target),
            BindingStateId {
                binding: binding_id,
                state: 0,
            },
        );
        self.registry
            .bindings_by_name
            .entry(name)
            .or_default()
            .push(binding_id);
        binding_id
    }

    /// Appends a source-ordered value to an existing binding, when the binding exists.
    fn add_binding_state(&mut self, binding_id: BindingId, fact: BindingFact) -> Option<usize> {
        let state_index = self
            .binding(binding_id)
            .map(|binding| binding.states.len())?;
        let state = BindingStateInfo {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: None,
            object_id: None,
            indexed_element_type: None,
            known_value: None,
            is_ring_generator: false,
            source_type: None,
            value_identity: None,
            value_range: fact.value,
            definition_range: fact.definition,
            span: fact.target,
            scope_idx: fact.scope,
        };
        if let Some(binding) = self.registry.bindings.get_mut(binding_id.0 as usize) {
            binding.states.push(state);
            self.registry.binding_states_by_range.insert(
                source_range_key(fact.target),
                BindingStateId {
                    binding: binding_id,
                    state: state_index,
                },
            );
            Some(state_index)
        } else {
            None
        }
    }

    /// Rebuilds source-range lookup entries after changing a binding's recorded states.
    fn reindex_binding_states(&mut self, binding_id: BindingId) {
        let ranges = self
            .binding(binding_id)
            .into_iter()
            .flat_map(|binding| binding.states.iter().map(|state| state.span))
            .collect::<Vec<_>>();
        for (state, range) in ranges.into_iter().enumerate() {
            self.registry.binding_states_by_range.insert(
                source_range_key(range),
                BindingStateId {
                    binding: binding_id,
                    state,
                },
            );
        }
    }

    /// The method function and installation an installing assignment makes,
    /// when it installs on a method function the analysis tracks.
    pub fn local_method_installation_signature<'a>(
        &'a self,
        assignment: &ExprAssign,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<(&'a FunctionInfo, &'a MethodInstallation)> {
        let installation = self.installation_for(assignment, source)?;
        let MethodHead::Function(name) = &installation.method.head else {
            return None;
        };
        let method = self.function_at(name.name(), installation.span.start)?;
        method
            .installations
            .contains(&installation.id)
            .then_some((method, installation))
    }

    /// Argument types and literal options of a call, in the scope where it is written.
    pub fn infer_call_static_facts(
        &self,
        call: &ExprOpBinAdj,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        let scope_idx = source
            .position_for_syntax(call)
            .and_then(|position| self.find_scope_at(position))
            .unwrap_or(0);
        TypeChecker::new(self, knowledge).call_facts(call, source, scope_idx, knowledge)
    }

    /// Source installations of a local callable that a call may reach.
    pub fn local_call_installations<'a>(
        &'a self,
        function: &'a FunctionInfo,
        call: &ExprOpBinAdj,
        position: Position,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<&'a MethodInstallation> {
        let facts = self.infer_call_static_facts(call, source, knowledge);
        TypeChecker::new(self, knowledge)
            .local_call_candidate_installation_ids(
                function,
                &facts.argument_types,
                position,
                knowledge,
            )
            .into_iter()
            .filter_map(|id| self.method_installation(id))
            .collect()
    }

    /// Source operator installations applicable to the inferred operand tuple.
    pub fn operator_call_installations<'a>(
        &'a self,
        operator: &Operator,
        arguments: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<&'a MethodInstallation> {
        let Some(function) = self.registry.operator_functions.get(operator) else {
            return Vec::new();
        };
        TypeChecker::new(self, knowledge)
            .local_call_candidate_installation_ids(function, arguments, position, knowledge)
            .into_iter()
            .filter_map(|id| self.method_installation(id))
            .collect()
    }

    /// Infers an expression's single result class, when one is known.
    pub fn infer_expression_static_type(
        &self,
        expression: &Expr,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        self.infer_expression_type(expression, source, knowledge)
            .single()
            .cloned()
    }

    /// Infers an expression's possible result types in the scope where it is written.
    pub fn infer_expression_type(
        &self,
        expression: &Expr,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let scope_idx = source
            .position_for_syntax(expression)
            .and_then(|position| self.find_scope_at(position))
            .unwrap_or(0);
        TypeChecker::new(self, knowledge).infer_type(expression, source, scope_idx)
    }

    /// Infers external call signature facts from expression syntax and positioned type evidence.
    pub fn infer_external_call_signature_facts(
        &self,
        callable: &ObjectName,
        facts: &CallStaticFacts,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallSignatureFacts> {
        TypeChecker::new(self, knowledge)
            .external_call_signature_facts(callable, facts, position, knowledge)
    }

    /// Associates an installation with its callable and records whether it takes effect.
    fn attach_method_installation(
        &mut self,
        installation: &mut MethodInstallation,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> MethodInstallationEffect {
        match &installation.method.head {
            MethodHead::Function(name) => {
                match self.callable_head_kind(name.name(), installation.span.start, knowledge) {
                    CallableHeadKind::PlainFunction => {
                        return MethodInstallationEffect::Rejected;
                    }
                    CallableHeadKind::Unknown => {
                        installation.options = MethodOptions::Unrecorded;
                        return MethodInstallationEffect::Unresolved;
                    }
                    CallableHeadKind::MethodFunction(options) => installation.options = options,
                }
                let Some(object_id) = self
                    .get_binding_at(name.name(), installation.span.start)
                    .and_then(|binding| binding.state.object_id)
                else {
                    // A library method function: the installation inherits its
                    // recorded `TypicalValue`.
                    if installation.method.codomain.is_none() {
                        installation.method.codomain = knowledge
                            .resolve_object(name)
                            .and_then(|indexed_id| knowledge.object(&indexed_id))
                            .and_then(|record| record.callable())
                            .and_then(|callable| callable.typical_value.as_ref())
                            .and_then(|type_id| knowledge.type_name(type_id))
                            .cloned();
                        installation.codomain_origin = installation
                            .method
                            .codomain
                            .as_ref()
                            .map(|_| CodomainOrigin::TypicalValue);
                    }
                    return MethodInstallationEffect::Effective;
                };
                let Some(function) = self.registry.callable_objects.get_mut(object_id.0) else {
                    return MethodInstallationEffect::Effective;
                };
                if installation.method.codomain.is_none() {
                    installation
                        .method
                        .codomain
                        .clone_from(&function.typical_value);
                    installation.codomain_origin = installation
                        .method
                        .codomain
                        .as_ref()
                        .map(|_| CodomainOrigin::TypicalValue);
                }
                function.installations.push(installation.id);
                MethodInstallationEffect::Effective
            }
            MethodHead::Operator(operator) => {
                let function = self
                    .registry
                    .operator_functions
                    .entry(operator.clone())
                    .or_insert_with(|| FunctionInfo {
                        typical_value: None,
                        installations: Vec::new(),
                        dispatch: None,
                        runtime_class: None,
                        indexed_callable: None,
                        kind: LocalFunctionKind::Plain,
                        declared_options: None,
                    });
                function.installations.push(installation.id);
                MethodInstallationEffect::Effective
            }
        }
    }

    /// Identity of the named binding visible at the requested source position.
    pub fn binding_id_at(&self, name: &str, pos: Position) -> Option<BindingId> {
        let scope_idx = self.find_scope_at(pos)?;
        self.binding_id_from_scope(name, scope_idx, pos)
    }
}
