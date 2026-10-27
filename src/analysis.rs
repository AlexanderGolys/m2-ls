//! Staged analysis of one immutable document snapshot.

use crate::node_metadata::TypedSource;
mod first_pass;
mod ring;
mod typechecker;

use m2_syn::nodes::{
    ElseClause, Expr, ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprNew as NewStatement, ExprOpBinIndex, ExprOpBinMember, ExprOpBinRegular as BinaryExpr,
    ExprOption as OptionExpression, ExprQuote as QuoteExpression, ExprWhile as WhileLoop,
    FloatLiteral, IntegerLiteral, List, LoopBody, RawStringLiteral, Sequence, StringLiteral,
    Symbol, ThenClause,
};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::ops::Deref;
use tower_lsp::lsp_types::{Position, Range as TextRange, SymbolKind};

use crate::builtin_index::{CallableKind, MethodSignature, Record};
use crate::meta::{BindingRole, Meta, Metadata};
use crate::node_metadata::{
    matches_token, token_spelling, visit_source_nodes, SyntaxKind, SyntaxNode,
};
use crate::object_registry::ObjectName;
use crate::object_registry::{ObjectId, OperatorForm, TypeData, TypeId};
use crate::package_index::{declared_package_name, PackageLoadingOption};
use crate::semantic_token::{syntax_semantic_token_type, SourceSemanticRole, SourceSemanticToken};
use crate::source::{ByteRange, SourceNavigation};
use crate::typesystem::{
    InferredType, LiteralOption, PositionedTypeKnowledge, SubtypeEvidence, Type, TypeKnowledge,
    TypeRole,
};
use crate::util::TextRangeExt;
use first_pass::{nested_symbols, package_name_range, walk, BindingEffect, BindingFact, ScopeTree};
pub(crate) use typechecker::TypeChecker;

/// Identity of one scoped symbol binding within an immutable analysis snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingStateId {
    binding: BindingId,
    state: usize,
}

/// An M2 operator — including `SPACE`, the juxtaposition operator (`X Y` is
/// `X SPACE Y`). Just another operator, not a special "adjacency" concept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Operator {
    pub token: ObjectName,
    pub form: OperatorForm,
}

impl Operator {
    fn from_expression(node: SyntaxNode<'_>) -> Option<Self> {
        let form = if node.is_adjacent_expr() || node.is_binary_expr() {
            OperatorForm::Binary
        } else if node.is_prefix_expr() {
            OperatorForm::Prefix
        } else if node.is_postfix_expr() {
            OperatorForm::Postfix
        } else {
            return None;
        };
        let token = match form {
            OperatorForm::Binary | OperatorForm::Assignment => node.binary_operator()?,
            OperatorForm::Prefix | OperatorForm::Postfix => node.operator_node()?.text(),
        };
        Some(Self {
            token: ObjectName::new(token),
            form,
        })
    }

    fn is_assignment(&self) -> bool {
        self.form == OperatorForm::Binary
            && (matches_token::<Token![=]>(self.token.name())
                || matches_token::<Token![:=]>(self.token.name())
                || matches_token::<Token![<-]>(self.token.name()))
    }

    fn is_option_assignment(&self) -> bool {
        self.form == OperatorForm::Binary && matches_token::<Token![=>]>(self.token.name())
    }
}

/// The callable or operator receiving an installed method.
/// Installation syntax affects the installed function's arity, not the
/// identity of this head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodHead {
    Function(ObjectName),
    Operator(Operator),
}

impl MethodHead {
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
    pub head: MethodHead,
    pub domain: Vec<ObjectName>,
    pub codomain: Option<ObjectName>,
}

impl Method {
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
    Variadic,
    Fixed(usize),
}

/// A characterized assignment that installs one named [`Method`].
///
/// It is produced once during analysis and consumed by every capability instead
/// of each re-deciding installation syntax. `span` covers the whole assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodInstallation {
    pub id: MethodInstallationId,
    pub method: Method,
    pub span: TextRange,
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
    pub(crate) syntax: MethodInstallationSyntax,
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
/// | `Options => {…}`, `Options => false` | `o -> x -> body` | `body`, `o` the option table |
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

    /// The lambda that receives the method's arguments in an installed
    /// function of the form these options expect, or `None` when the form
    /// does not fit. With unrecorded options a lambda returning another lambda
    /// could be either form, so only one that does not is taken.
    fn argument_lambda(self, installed: InstalledFunction<'_>) -> Option<SyntaxNode<'_>> {
        match (self, installed) {
            (Self::Absent, InstalledFunction::Lambda(lambda)) => Some(lambda),
            (Self::Unrecorded, InstalledFunction::Lambda(lambda)) => {
                returned_lambda(lambda).is_none().then_some(lambda)
            }
            (Self::Table | Self::TableOrForwarded, InstalledFunction::Lambda(handler))
            | (
                Self::Absent | Self::Forwarded | Self::TableOrForwarded | Self::Unrecorded,
                InstalledFunction::OptionDeclaring { handler, .. },
            ) => returned_lambda(handler),
            _ => None,
        }
    }
}

/// The lambda a lambda returns directly, as `x -> …` in `o -> x -> …`.
fn returned_lambda(lambda: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    lambda
        .lambda_body()
        .and_then(parenthesized_value)
        .filter(|body| body.is::<LambdaExpression>())
}

/// The function an installation stores, read from its syntax once grouping
/// parentheses and a `Codomain =>` annotation are removed.
#[derive(Debug, Clone, Copy)]
enum InstalledFunction<'tree> {
    /// `x -> …`, or `o -> x -> …` receiving an option table.
    Lambda(SyntaxNode<'tree>),
    /// `{…} >> o -> …`: a function declaring its own options, whose handler
    /// `o -> …` receives them.
    OptionDeclaring {
        declaration: SyntaxNode<'tree>,
        handler: SyntaxNode<'tree>,
    },
    /// Any other value, such as a named function.
    Other(SyntaxNode<'tree>),
}

impl<'tree> InstalledFunction<'tree> {
    fn of(value: SyntaxNode<'tree>) -> Option<Self> {
        let value = parenthesized_value(value)?;
        if value.is::<LambdaExpression>() {
            return Some(Self::Lambda(value));
        }
        if value.is_option_assignment() {
            return Self::of(value.binary_parts().map(|parts| parts.right)?);
        }
        let handler = value
            .has_binary_operator::<Token![>>]>()
            .then(|| {
                value
                    .binary_parts()
                    .map(|parts| parts.right)
                    .and_then(parenthesized_value)
            })
            .flatten()
            .filter(|handler| handler.is::<LambdaExpression>());
        Some(match handler {
            Some(handler) => Self::OptionDeclaring {
                declaration: value,
                handler,
            },
            None => Self::Other(value),
        })
    }

    /// The whole installed function, before which a codomain annotation goes.
    fn node(self) -> SyntaxNode<'tree> {
        match self {
            Self::Lambda(node)
            | Self::OptionDeclaring {
                declaration: node, ..
            }
            | Self::Other(node) => node,
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

impl OptionDeclaration {
    /// The declaration the list holding `entry`, an option `key => default`,
    /// makes, or `None` when that list declares no options.
    pub fn of_entry(entry: SyntaxNode<'_>) -> Option<Self> {
        let list = entry.parent().filter(|parent| parent.is::<List>())?;
        let holder = list.parent()?;
        let holds_list =
            |side: Option<SyntaxNode<'_>>| side.is_some_and(|side| side.id() == list.id());
        let parts = holder.binary_parts();
        if holds_list(parts.map(|parts| parts.left))
            && matches!(
                InstalledFunction::of(holder),
                Some(InstalledFunction::OptionDeclaring { .. })
            )
        {
            return Some(Self::Function);
        }
        holder
            .ancestors()
            .find_map(MethodDeclaration::of)
            .filter(|method| holds_list(method.option(MethodDeclarationOption::Options)))
            .map(|_| Self::Method)
    }
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

/// An installation read from its syntax, before its head is resolved.
struct ClassifiedInstallation<'tree> {
    installation: MethodInstallation,
    method_type_spans: Vec<(TextRange, SourceSemanticRole)>,
    installed: Option<InstalledFunction<'tree>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MethodInstallationSyntax {
    Classical,
    InstallMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MethodInstallationEffect {
    Rejected,
    Unresolved,
    Effective,
}

/// A lambda-body codomain that can safely drive an annotation quick fix.
pub struct MethodCodomainDeduction {
    pub codomain: ObjectName,
    pub annotated_codomain: Option<ObjectName>,
    pub diagnostic_range: TextRange,
    pub edit: MethodCodomainEdit,
}

/// The source edit needed to make a method codomain annotation precise.
pub enum MethodCodomainEdit {
    Add(TextRange),
    Replace,
}

/// The outline-relevant semantic shape of one characterized assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentFactKind {
    MethodInstallation(MethodInstallationId),
    IndexedVariable,
    ScopedCallable,
}

/// Source-facing assignment facts retained by analysis for capability projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentFact {
    pub label: String,
    pub span: TextRange,
    pub target_span: TextRange,
    pub value_span: Option<TextRange>,
    pub scope_idx: usize,
    pub kind: AssignmentFactKind,
}

impl MethodInstallation {
    /// The argument count the lambda receiving the method's arguments must take.
    pub fn expected_argument_arity(&self) -> usize {
        self.expected_argument_arity
    }

    pub fn takes_effect(&self) -> bool {
        self.effect == MethodInstallationEffect::Effective
    }

    pub fn is_workspace_candidate(&self) -> bool {
        self.effect != MethodInstallationEffect::Rejected
    }
}

/// Syntax-derived callable behavior used to decide whether method
/// installations take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalFunctionKind {
    Plain,
    Method(MethodOptions),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallableHeadKind {
    PlainFunction,
    MethodFunction(MethodOptions),
    Unknown,
}

/// Semantic information about one statically tracked callable.
///
/// Installed methods are referenced by identity in the snapshot's semantic
/// registry
/// so their source facts have one owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionInfo {
    pub typical_value: Option<ObjectName>,
    pub installations: Vec<MethodInstallationId>,
    pub dispatch: Option<Dispatch>,
    runtime_class: Option<TypeRole>,
    indexed_callable: Option<ObjectName>,
    kind: LocalFunctionKind,
}

impl FunctionInfo {
    pub fn is_method_function(&self) -> bool {
        matches!(self.kind, LocalFunctionKind::Method(_))
    }
}

/// Static facts computed for one call after separating positional arguments
/// from literal option assignments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallStaticFacts {
    pub argument_types: Vec<InferredType>,
    pub literal_options: Vec<LiteralOption>,
}

/// Installed signatures classified by the typechecker's dispatch-range result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallSignatureFacts {
    pub pinned: Option<MethodSignature>,
    pub possible: Vec<MethodSignature>,
    pub excluded: Vec<MethodSignature>,
}

/// Source-independent identity and editor-facing anchor of one lexical binding.
///
/// Its value and inferred type at a particular point live in
/// [`BindingStateInfo`] records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingInfo {
    pub binding_id: BindingId,
    pub name: ObjectName,
    pub role: BindingRole,
    pub potential_export: bool,
    pub range: TextRange,
    pub scope_idx: usize,
    pub states: Vec<BindingStateInfo>,
    introduced_by_assignment: bool,
}

impl BindingInfo {
    /// Whether the binding is a local of a function or block: a parameter,
    /// or a name declared with `:=` inside one. A name assigned with `=`
    /// inside a function, with no such local in scope, is the global.
    pub fn is_local(&self) -> bool {
        self.role == BindingRole::Parameter
            || (self.scope_idx != 0 && !self.introduced_by_assignment)
    }
}

/// One source-ordered value and inferred-type state of a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingStateInfo {
    pub presentation_kind: SymbolKind,
    pub inferred_type: Option<InferredType>,
    pub object_id: Option<CallableObjectId>,
    pub indexed_element_type: Option<ring::IndexedVariableTypes>,
    pub is_ring_generator: bool,
    pub source_type: Option<TypeId>,
    pub value_range: Option<TextRange>,
    pub definition_range: TextRange,
    pub span: TextRange,
    pub scope_idx: usize,
}

impl BindingStateInfo {
    fn effective_from(&self) -> Position {
        self.value_range.map_or(self.span.end, |value| value.end)
    }
}

/// A binding declaration paired with the state effective at a query position.
#[derive(Debug, Clone, Copy)]
pub struct BindingView<'a> {
    pub binding: &'a BindingInfo,
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
#[derive(Debug, Default)]
struct SourceTypeFacts {
    data: HashMap<TypeId, TypeData>,
}

type SourceRangeKey = [u32; 4];

fn source_range_key(range: TextRange) -> SourceRangeKey {
    [
        range.start.line,
        range.start.character,
        range.end.line,
        range.end.character,
    ]
}

fn source_cell(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let root = node.root();
    node.ancestors().find(|ancestor| {
        ancestor
            .parent()
            .is_some_and(|parent| parent.id() == root.id())
    })
}

fn is_value_cell(node: SyntaxNode<'_>) -> bool {
    node.is_source_cell() && !node.is_muted_statement()
}

/// Canonical per-snapshot store of symbols, bindings, scopes, and their indexes.
#[derive(Debug, Default)]
struct SemanticRegistry {
    scopes: ScopeTree,
    bindings: Vec<BindingInfo>,
    bindings_by_name: HashMap<ObjectName, Vec<BindingId>>,
    binding_states_by_range: HashMap<SourceRangeKey, BindingStateId>,
    callable_objects: Vec<FunctionInfo>,
    indexed_callable_objects: HashMap<ObjectId, CallableObjectId>,
    operator_functions: HashMap<Operator, FunctionInfo>,
    installations: Vec<MethodInstallation>,
    /// Function-head installations by the lambda receiving their arguments,
    /// whose parameters take the installation's domain types.
    installations_by_argument_lambda: HashMap<SourceRangeKey, MethodInstallationId>,
    assignment_facts: Vec<AssignmentFact>,
    pending_source_semantic_roles: Vec<(TextRange, SourceSemanticRole)>,
    source_semantic_tokens: Vec<SourceSemanticToken>,
    source_types: SourceTypeFacts,
    ring_generators: HashMap<ObjectName, Vec<ring::RingGenerator>>,
    /// Where ring generators are declared, in the order their bindings were
    /// made: each bound constructor's variable list and `Variable` option.
    ring_generator_specs: Vec<TextRange>,
}

#[derive(Clone, Copy)]
pub enum OutputReference {
    Relative(NonZeroUsize),
    Absolute(NonZeroUsize),
    MissingAbsolute,
}

impl OutputReference {
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

    pub fn referenced_value<'tree>(self, node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let cell = source_cell(node)?;
        if !is_value_cell(cell) {
            return None;
        }
        let root = cell.root();
        let preceding_cells = root
            .named_children()
            .filter(|candidate| {
                is_value_cell(*candidate) && candidate.end_byte() <= cell.start_byte()
            })
            .collect::<Vec<_>>();

        match self {
            Self::Relative(distance) => preceding_cells
                .iter()
                .rev()
                .filter_map(SyntaxNode::final_value_child)
                .nth(distance.get() - 1),
            Self::Absolute(number) => preceding_cells
                .get(number.get() - 1)
                .and_then(SyntaxNode::final_value_child),
            Self::MissingAbsolute => None,
        }
    }
}

#[derive(Debug, Clone)]
struct BindingEnrichment {
    presentation_kind: SymbolKind,
    inferred_type: Option<InferredType>,
    object_id: Option<CallableObjectId>,
    indexed_element_type: Option<ring::IndexedVariableTypes>,
    parent_type: Option<TypeId>,
}

/// The parsed loop or function body receiving a control transfer.
#[derive(Clone, Copy)]
pub enum ControlTransferTarget<'tree> {
    Function(SyntaxNode<'tree>),
    ListLoop(SyntaxNode<'tree>),
    DoLoop(SyntaxNode<'tree>),
    LoopCallback {
        function: SyntaxNode<'tree>,
        callable: SyntaxNode<'tree>,
    },
}

impl<'tree> ControlTransferTarget<'tree> {
    pub fn owner(self) -> SyntaxNode<'tree> {
        match self {
            Self::Function(owner) | Self::ListLoop(owner) | Self::DoLoop(owner) => owner,
            Self::LoopCallback { function, .. } => function,
        }
    }

    pub fn accepts(self, transfer: SyntaxNode<'_>) -> bool {
        match self {
            Self::Function(_) => transfer.is_return_expr(),
            Self::ListLoop(_) => transfer.is_break_expr() || transfer.is_continue_expr(),
            Self::LoopCallback { .. } => transfer.is_break_expr(),
            Self::DoLoop(_) => {
                transfer.is_break_expr()
                    || transfer.is_continue_expr() && transfer.control_transfer_value().is_none()
            }
        }
    }
}

/// Complete semantic analysis of one immutable document snapshot.
/// It owns source facts and characterized method installations; expression
/// types are not retained after inference.
#[derive(Debug)]
pub struct Analysis {
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

    pub fn get_binding_at(&self, name: &str, pos: Position) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(pos)?;
        self.get_binding_from_scope(name, scope_idx, pos)
    }

    pub fn visible_source_binding_at(
        &self,
        name: &str,
        pos: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(pos)?;
        self.visible_source_binding_from_scope(name, scope_idx, pos, knowledge)
    }

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

    pub fn source_binding_is_visible(
        binding: BindingView<'_>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        binding.scope_idx != 0 || !knowledge.shadows_source(&binding.name, binding.state.span.start)
    }

    pub fn control_transfer_target<'tree>(
        &self,
        transfer: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ControlTransferTarget<'tree>> {
        if !transfer.is_control_transfer() {
            return None;
        }

        let mut direct_child = transfer;
        while let Some(parent) = direct_child.parent() {
            if transfer.is_return_expr() && parent.is::<LambdaExpression>() {
                return parent
                    .lambda_body()
                    .is_some_and(|body| body.contains(transfer))
                    .then_some(ControlTransferTarget::Function(parent));
            }
            if (transfer.is_break_expr() || transfer.is_continue_expr())
                && parent.is::<LambdaExpression>()
            {
                let callable = direct_callback_callable(parent)?;
                let name = callable.text();
                let position = source.position_for_node(callable);
                if self
                    .visible_source_binding_at(name, position, knowledge)
                    .is_some()
                    || knowledge
                        .get_record(&ObjectName::new(name))
                        .and_then(|record| record.callable())
                        .is_none()
                {
                    return None;
                }
                return match name {
                    "apply" | "scan" => Some(ControlTransferTarget::LoopCallback {
                        function: parent,
                        callable,
                    }),
                    _ => None,
                };
            }
            if (parent.is::<ForLoop>() || parent.is::<WhileLoop>()) && direct_child.is::<LoopBody>()
            {
                if direct_child
                    .listed_value()
                    .is_some_and(|value| value.contains(transfer))
                {
                    return Some(ControlTransferTarget::ListLoop(parent));
                }
                if direct_child
                    .ignored_value()
                    .is_some_and(|value| value.contains(transfer))
                {
                    return Some(ControlTransferTarget::DoLoop(parent));
                }
                return None;
            }
            direct_child = parent;
        }
        None
    }

    pub fn for_each_control_transfer<'tree>(
        &self,
        root: SyntaxNode<'tree>,
        syntax: Option<&TypedSource>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        mut visit: impl FnMut(SyntaxNode<'tree>, ControlTransferTarget<'tree>),
    ) {
        visit_source_nodes(root, syntax, |node| {
            if node.is_control_transfer() {
                if let Some(target) = self.control_transfer_target(node, source, knowledge) {
                    visit(node, target);
                }
            }
        });
    }

    fn get_binding_from_scope(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
    ) -> Option<BindingView<'_>> {
        let binding_id = self.binding_id_from_scope(name, scope_idx, pos)?;
        self.binding_state_from_scope(binding_id, scope_idx, pos)
    }

    fn binding_id_from_scope(
        &self,
        name: &str,
        scope_idx: usize,
        pos: Position,
    ) -> Option<BindingId> {
        self.binding_id_from_scope_in(name, scope_idx, pos, &self.registry.scopes)
    }

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

    fn binding(&self, binding_id: BindingId) -> Option<&BindingInfo> {
        self.registry.bindings.get(binding_id.0 as usize)
    }

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

    pub fn future_assignment_binding_at(
        &self,
        name: &str,
        position: Position,
    ) -> Option<BindingView<'_>> {
        let scope_idx = self.find_scope_at(position)?;
        let binding_id = self.future_assignment_binding(name, scope_idx, position)?;
        self.binding_anchor(binding_id)
    }

    fn binding_anchor(&self, binding_id: BindingId) -> Option<BindingView<'_>> {
        let binding = self.binding(binding_id)?;
        let state = binding.states.first()?;
        Some(BindingView { binding, state })
    }

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

    pub fn function_at(&self, name: &str, position: Position) -> Option<&FunctionInfo> {
        let binding = self.get_binding_at(name, position)?;
        self.function_for_binding(binding)
    }

    pub fn method_installation_codomain<'a>(
        &'a self,
        installation: &'a MethodInstallation,
    ) -> Option<&'a str> {
        installation.method.codomain.as_ref().map(ObjectName::name)
    }

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

    pub fn method_installation(&self, id: MethodInstallationId) -> Option<&MethodInstallation> {
        self.registry.installations.get(id.0)
    }

    pub fn installations(&self) -> &[MethodInstallation] {
        &self.registry.installations
    }

    pub fn assignment_facts(&self) -> &[AssignmentFact] {
        &self.registry.assignment_facts
    }

    pub fn scope_with_range(&self, range: TextRange) -> Option<usize> {
        self.registry.scopes.with_range(range)
    }

    pub fn scope_count(&self) -> usize {
        self.registry.scopes.len()
    }

    pub fn parent_scope(&self, scope_idx: usize) -> Option<usize> {
        self.registry.scopes.parent(scope_idx)
    }

    pub fn source_semantic_tokens(&self) -> &[SourceSemanticToken] {
        &self.registry.source_semantic_tokens
    }

    pub fn bindings_in_scope(&self, scope_idx: usize) -> impl Iterator<Item = BindingView<'_>> {
        self.bindings()
            .filter(move |binding| binding.scope_idx == scope_idx)
    }

    pub fn bindings(&self) -> impl Iterator<Item = BindingView<'_>> {
        self.registry
            .bindings
            .iter()
            .filter_map(|binding| self.binding_anchor(binding.binding_id))
    }

    pub fn binding_states(&self) -> impl Iterator<Item = BindingView<'_>> {
        self.registry.bindings.iter().flat_map(|binding| {
            binding
                .states
                .iter()
                .map(|state| BindingView { binding, state })
        })
    }

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

    pub(crate) fn find_scope_at(&self, pos: Position) -> Option<usize> {
        self.registry.scopes.at(pos)
    }

    pub fn new_with_knowledge(
        root: SyntaxNode<'_>,
        syntax: Option<&TypedSource>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
    ) -> Self {
        let mut analysis = Analysis {
            registry: SemanticRegistry::default(),
        };
        let syntax = syntax.unwrap_or_else(|| root.typed_source());
        let source = crate::source::SyntaxNavigation::new(source, root.snapshot());
        let bindings_timing = crate::telemetry::Timing::start(
            "analysis.bindings",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.registry.scopes = walk(syntax, root, &source, |fact, scopes| {
            analysis.collect_binding_fact(fact, scopes);
        });
        drop(bindings_timing);
        let inference_timing = crate::telemetry::Timing::start(
            "analysis.inference",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.enrich_types_typed(syntax, root, &source, knowledge);
        drop(inference_timing);
        let _semantics_timing = crate::telemetry::Timing::start(
            "analysis.semantic_tokens",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        analysis.collect_semantics(root, Some(syntax), &source, knowledge);
        analysis.registry.pending_source_semantic_roles.clear();
        analysis
    }

    fn collect_binding_fact(&mut self, fact: BindingFact, scopes: &ScopeTree) {
        self.collect_definition(fact, scopes);
    }

    fn collect_semantics(
        &mut self,
        root: SyntaxNode,
        syntax: Option<&TypedSource>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        if let Some(syntax) = syntax {
            syntax.visit(&mut TypedSemanticRoles {
                analysis: self,
                root,
                source,
                knowledge_provider,
            });
        }
        let mut covered_until = 0;
        for node in root.descendants() {
            if node.start_byte() < covered_until {
                continue;
            }
            let knowledge = knowledge_provider.at_position(source.position_for_node(node));
            if self.is_indexed_ring_generator(node, source) {
                covered_until = node.end_byte();
                if let Some(base) = node
                    .binary_parts()
                    .and_then(|parts| parenthesized_value(parts.left))
                {
                    if let Some((_, quote)) = base.quote_parts() {
                        self.record_source_semantic_token(quote, source, &knowledge);
                    }
                }
                let span = source.span_for_bytes(Self::indexed_ring_generator_bytes(node));
                self.registry
                    .source_semantic_tokens
                    .push(SourceSemanticToken {
                        source_role: Some(self.ring_generator_role(span.range())),
                        span,
                        syntax_token_type: None,
                        is_symbol: false,
                        is_unquoted_symbol: false,
                        is_expression_symbol: false,
                        is_condition_value: false,
                    });
                continue;
            }
            self.record_source_semantic_token(node, source, &knowledge);
        }
    }

    /// A quote yields its operand as a `Symbol` instead of evaluating it, so the
    /// quoted name reads as a symbol regardless of what it is bound to.
    fn record_quoted_symbol_role(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        if !node.is::<QuoteExpression>() {
            return;
        }
        if let Some(quoted) = node.quoted_symbol() {
            self.register_source_semantic_role(
                source.range_for_node(quoted),
                SourceSemanticRole::QuotedSymbol,
            );
        }
    }

    fn register_option_key(&mut self, key: SyntaxNode, source: &(impl SourceNavigation + ?Sized)) {
        self.register_source_semantic_role(
            source.range_for_node(key),
            SourceSemanticRole::OptionKey,
        );
    }

    fn record_namespace_role(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if (node.is::<StringLiteral>() || node.is::<RawStringLiteral>())
            && (indexed_string_names_package(node, knowledge)
                || PackageLoadingOption::lists_package(node))
        {
            self.register_source_semantic_role(
                source.range_for_node(node),
                SourceSemanticRole::NamespaceArgument,
            );
        }
    }

    fn register_source_semantic_role(&mut self, range: TextRange, role: SourceSemanticRole) {
        if self
            .registry
            .pending_source_semantic_roles
            .iter()
            .all(|(registered, _)| *registered != range)
        {
            self.registry
                .pending_source_semantic_roles
                .push((range, role));
        }
    }

    fn set_source_semantic_role(&mut self, range: TextRange, role: SourceSemanticRole) {
        if let Some((_, registered)) = self
            .registry
            .pending_source_semantic_roles
            .iter_mut()
            .find(|(registered, _)| *registered == range)
        {
            *registered = role;
        } else {
            self.registry
                .pending_source_semantic_roles
                .push((range, role));
        }
    }

    fn record_source_semantic_token(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if node.is::<Symbol>() && OutputReference::parse(node.text()).is_some() {
            let position = source.position_for_node(node);
            if self
                .visible_source_binding_at(node.text(), position, knowledge)
                .is_none()
            {
                return;
            }
        }
        let syntax_token_type = syntax_semantic_token_type(node);
        let is_symbol = symbol_node_text(node).is_some();
        if !is_symbol && syntax_token_type.is_none() {
            return;
        }

        let span = source.span_for_node(node);
        let is_ring_generator = is_symbol
            && self
                .visible_source_binding_at(node.text(), span.range().start, knowledge)
                .is_some_and(|binding| binding.state.is_ring_generator);
        let generator_role = is_ring_generator.then(|| self.ring_generator_role(span.range()));
        let source_role = generator_role
            .clone()
            .filter(|role| *role == SourceSemanticRole::RingGeneratorDeclaration)
            .or_else(|| {
                self.registry
                    .pending_source_semantic_roles
                    .iter()
                    .find(|(range, _)| *range == span.range())
                    .map(|(_, role)| role.clone())
            })
            .or(generator_role);
        self.registry
            .source_semantic_tokens
            .push(SourceSemanticToken {
                span,
                syntax_token_type,
                source_role,
                is_symbol,
                is_unquoted_symbol: node.is::<Symbol>(),
                is_expression_symbol: is_expression_symbol(node),
                is_condition_value: is_condition_value(node),
            });
    }

    /// The role of a ring generator written at `range`: declared there when
    /// it lies in a constructor's variables, otherwise referred to.
    fn ring_generator_role(&self, range: TextRange) -> SourceSemanticRole {
        if self.declares_ring_generators(range) {
            SourceSemanticRole::RingGeneratorDeclaration
        } else {
            SourceSemanticRole::RingGenerator
        }
    }

    fn record_assignment_fact(
        &mut self,
        assignment: SyntaxNode,
        target: SyntaxNode,
        value: Option<SyntaxNode>,
        scope_idx: usize,
        installation_id: Option<MethodInstallationId>,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let kind = if let Some(id) = installation_id {
            AssignmentFactKind::MethodInstallation(id)
        } else {
            if !assignment.has_binary_operator::<Token![=]>() {
                return;
            }
            if target.has_binary_operator::<Token![_]>() {
                AssignmentFactKind::IndexedVariable
            } else if target.binary_operator().is_some() {
                AssignmentFactKind::ScopedCallable
            } else {
                return;
            }
        };
        let target_end = if let Some(installation) =
            assignment.get::<m2_syn::nodes::ExprAssignInstallMethod>()
        {
            assignment
                .related(installation.signature.as_ref())
                .map(|node| node.trimmed_end_byte())
                .unwrap_or_else(|| target.trimmed_end_byte())
        } else if target.is::<NewStatement>() {
            assignment_parts(assignment)
                .map(|(_, operator, _)| operator.start_byte())
                .unwrap_or_else(|| target.trimmed_end_byte())
        } else {
            target.trimmed_end_byte()
        };
        let target_span = source.range_for_bytes(target.start_byte()..target_end);
        let label = source.text()[target.start_byte()..target_end]
            .trim_end()
            .to_string();
        self.registry.assignment_facts.push(AssignmentFact {
            label,
            span: source.range_for_node(assignment),
            target_span,
            value_span: value.map(|value| source.range_for_node(value)),
            scope_idx,
            kind,
        });
    }

    /// The installation characterized for the assignment spanning `node`, if any.
    /// The domain of the installation whose arguments `lambda` receives.
    fn argument_lambda_domain(
        &self,
        lambda: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&Vec<ObjectName>> {
        let id = self
            .registry
            .installations_by_argument_lambda
            .get(&source_range_key(source.range_for_node(lambda)))?;
        Some(&self.registry.installations[id.0].method.domain)
    }

    pub fn installation_for(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<&MethodInstallation> {
        let span = source.range_for_node(node);
        self.registry
            .installations
            .iter()
            .find(|installation| installation.span == span)
    }

    /// Characterize an `=`/`:=` assignment as a method installation, or `None`
    /// when it is an ordinary assignment/call. The single source of truth for
    /// the install-vs-call distinction.
    ///
    /// - `:=` installs whenever the left side matches an installation shape (the
    ///   six forms: `f Type`, `f (T..)`, `T1 op T2`, `T1 T2` adjacency,
    ///   `prefix T`, `T postfix`).
    /// - `=` installs ONLY the assignment-operator form `T1 op T2 = f`, and only
    ///   when both operands are types — otherwise the identical syntax assigns
    ///   `f` to the lvalue `T1 op T2`, which is a call.
    fn classify_installation<'tree>(
        &self,
        id: MethodInstallationId,
        node: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ClassifiedInstallation<'tree>> {
        let (left, operator_node, right) = assignment_parts(node)?;
        let operator = operator_node.text();
        let position = source.position_for_node(left);
        let (head, domain_nodes) =
            if let Some(installation) = node.get::<m2_syn::nodes::ExprAssignInstallMethod>() {
                let method = node.related(installation.method.as_ref())?;
                let signature = node.related(installation.signature.as_ref())?;
                let name = symbol_node_text(method)?;
                if self.operand_is_type(name, position, knowledge) {
                    symbol_node_text(signature)?;
                    (
                        MethodHead::Operator(Operator {
                            token: ObjectName::new("SPACE"),
                            form: OperatorForm::Binary,
                        }),
                        vec![method, signature],
                    )
                } else {
                    (
                        MethodHead::Function(ObjectName::new(name)),
                        method_installation_domain_nodes(signature)?,
                    )
                }
            } else {
                self.installation_shape(left, position, knowledge)?
            };
        let domain = domain_nodes
            .iter()
            .map(|node| ObjectName::new(symbol_node_text(*node).unwrap_or_else(|| node.text())))
            .collect::<Vec<_>>();
        let domain_spans = domain_nodes
            .iter()
            .map(|node| source.range_for_node(*node))
            .collect::<Vec<_>>();
        let operand_arity = domain.len();
        let span = source.range_for_node(node);
        let codomain_node = method_codomain_annotation(right);
        let codomain = codomain_node
            .and_then(symbol_node_text)
            .map(ObjectName::new);
        let codomain_span = codomain_node.map(|node| source.range_for_node(node));
        // What the installation installs on: the method function named on
        // the left, or the operator written between its operands.
        let head_node = match &head {
            MethodHead::Function(name) => left
                .descendants()
                .find(|node| node.is::<Symbol>() && node.text() == name.name()),
            MethodHead::Operator(_) => left.operator_node(),
        };
        let method_semantic_spans = domain_spans
            .iter()
            .map(|range| (*range, SourceSemanticRole::MethodTypeParameter))
            .chain(codomain_span.map(|range| (range, SourceSemanticRole::CodomainAnnotation)))
            .chain(head_node.map(|node| {
                (
                    source.range_for_node(node),
                    SourceSemanticRole::InstallationHead,
                )
            }))
            .collect::<Vec<_>>();
        if matches_token::<Token![:=]>(operator) {
            // `:=` installs by shape alone — no type check on the operands.
            Some(ClassifiedInstallation {
                installation: MethodInstallation {
                    id,
                    codomain_origin: codomain.is_some().then_some(CodomainOrigin::Annotation),
                    method: Method::new(head, domain, codomain),
                    span,
                    expected_argument_arity: operand_arity,
                    options: MethodOptions::Absent,
                    installed: InstalledValue::Unknown,
                    installed_lambda_dispatch: None,
                    argument_lambda_dispatch: None,
                    syntax: MethodInstallationSyntax::Classical,
                    effect: MethodInstallationEffect::Rejected,
                },
                method_type_spans: method_semantic_spans,
                installed: InstalledFunction::of(right),
            })
        } else if matches_token::<Token![=]>(operator) {
            // `=` installs only the assignment form of a BINARY operator (incl.
            // SPACE), and only when every operand is a type; otherwise the same
            // syntax assigns to the lvalue `X op Y`, which is a call.
            match head {
                MethodHead::Operator(op)
                    if op.form == OperatorForm::Binary
                        && domain.iter().all(|operand| {
                            self.operand_is_type(operand.name(), position, knowledge)
                        }) =>
                {
                    Some(ClassifiedInstallation {
                        installation: MethodInstallation {
                            id,
                            codomain_origin: codomain
                                .is_some()
                                .then_some(CodomainOrigin::Annotation),
                            method: Method::new(MethodHead::Operator(op), domain, codomain),
                            span,
                            expected_argument_arity: operand_arity + 1,
                            options: MethodOptions::Absent,
                            installed: InstalledValue::Unknown,
                            installed_lambda_dispatch: None,
                            argument_lambda_dispatch: None,
                            syntax: MethodInstallationSyntax::Classical,
                            effect: MethodInstallationEffect::Rejected,
                        },
                        method_type_spans: method_semantic_spans,
                        installed: InstalledFunction::of(right),
                    })
                }
                _ => None,
            }
        } else {
            None
        }
    }

    /// Classify the left side of an assignment into a `(MethodHead, domain)`
    /// pair (the bare, non-assignment head), or `None` if it is not an
    /// installation target at all. The `=`/`:=` rule is applied by the caller.
    fn classify_install_method<'tree>(
        &self,
        id: MethodInstallationId,
        node: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ClassifiedInstallation<'tree>> {
        if !node.is_space_application()
            || node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(symbol_node_text)
                != Some("installMethod")
        {
            return None;
        }
        let arguments = node.binary_parts().map(|parts| parts.right)?;
        let mut arguments = arguments.collection_elements().collect::<Vec<_>>();
        if arguments.len() < 2 {
            return None;
        }
        let value = arguments.pop()?;
        let head_node = arguments.remove(0);
        let domain_nodes = arguments;
        let domain = domain_nodes
            .iter()
            .map(|node| symbol_node_text(*node).map(ObjectName::new))
            .collect::<Option<Vec<_>>>()?;
        let head = self.install_method_head(head_node, domain.len(), knowledge)?;
        let expected_argument_arity = match &head {
            // `<-` methods are installed like `(op, =)` assignment methods: the
            // assigned value is one extra parameter after the domain.
            MethodHead::Operator(operator)
                if matches_token::<Token![<-]>(operator.token.name()) =>
            {
                domain.len() + 1
            }
            MethodHead::Operator(operator) if operator.form == OperatorForm::Binary => 2,
            MethodHead::Function(_) | MethodHead::Operator(_) => domain.len(),
        };
        let method_semantic_spans = domain_nodes
            .iter()
            .map(|node| {
                (
                    source.range_for_node(*node),
                    SourceSemanticRole::MethodTypeParameter,
                )
            })
            .collect();

        Some(ClassifiedInstallation {
            installation: MethodInstallation {
                id,
                method: Method::new(head, domain, None),
                codomain_origin: None,
                span: source.range_for_node(node),
                expected_argument_arity,
                options: MethodOptions::Absent,
                installed: InstalledValue::Unknown,
                installed_lambda_dispatch: None,
                argument_lambda_dispatch: None,
                syntax: MethodInstallationSyntax::InstallMethod,
                effect: MethodInstallationEffect::Rejected,
            },
            method_type_spans: method_semantic_spans,
            installed: InstalledFunction::of(value),
        })
    }

    fn install_method_head(
        &self,
        node: SyntaxNode,
        domain_arity: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodHead> {
        if let Some(name) = symbol_node_text(node) {
            return Some(MethodHead::Function(ObjectName::new(name)));
        }
        if !node.is::<QuoteExpression>() {
            return None;
        }
        let token = node.quoted_symbol().or_else(|| node.named_child(0))?;
        let token = ObjectName::new(token.text());
        let operator = knowledge.get_record(&token)?.operator_info()?;
        let supports = |form| operator.forms.iter().any(|candidate| candidate == form);
        let form = match domain_arity {
            2.. if supports("Binary") => OperatorForm::Binary,
            1 if supports("Prefix") => OperatorForm::Prefix,
            1 if supports("Postfix") => OperatorForm::Postfix,
            1 if supports("Binary") => OperatorForm::Binary,
            _ => return None,
        };
        Some(MethodHead::Operator(Operator { token, form }))
    }

    pub(crate) fn installation_shape<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<(MethodHead, Vec<SyntaxNode<'tree>>)> {
        // A parenthesized expression is identified with its final value, so
        // `(T op S) := f` installs exactly like `T op S := f`. A final `muted`
        // child means the group evaluates to null and is not an installation
        // target.
        if node.is_holder() {
            let inner = node.final_value_child()?;
            return self.installation_shape(inner, position, knowledge);
        }
        if node.is::<NewStatement>() {
            let target_type = new_statement_value(node.new_class()?)?;
            symbol_node_text(target_type)?;
            let mut domain = vec![target_type];
            for value in [new_statement_parent(node), new_statement_instance(node)]
                .into_iter()
                .flatten()
            {
                domain.extend(method_installation_domain_nodes(value)?);
            }
            return Some((
                MethodHead::Operator(Operator {
                    token: ObjectName::new(token_spelling::<Token![new]>()),
                    form: OperatorForm::Prefix,
                }),
                domain,
            ));
        }
        if node.is_adjacent_expr() || node.is_binary_expr() {
            let left = node.binary_parts().map(|parts| parts.left)?;
            let right = node.binary_parts().map(|parts| parts.right)?;
            let operator = Operator::from_expression(node)?;
            if node.is_space_application() {
                // `A B` (juxtaposition = the SPACE operator): a method on the
                // named function `A` when `A` is a function, or a SPACE
                // operator method on the type pair when `A` is a type.
                let left_name = symbol_node_text(left)?;
                if self.operand_is_type(left_name, position, knowledge) {
                    symbol_node_text(right)?;
                    Some((MethodHead::Operator(operator), vec![left, right]))
                } else {
                    Some((
                        MethodHead::Function(ObjectName::new(left_name)),
                        method_installation_domain_nodes(right)?,
                    ))
                }
            } else {
                // `X op Y`: an explicit binary-operator method.
                if operator.is_assignment() || operator.is_option_assignment() {
                    return None;
                }
                symbol_node_text(left)?;
                symbol_node_text(right)?;
                Some((MethodHead::Operator(operator), vec![left, right]))
            }
        } else if node.is_prefix_expr() || node.is_postfix_expr() {
            let operand = node.unary_operand()?;
            symbol_node_text(operand)?;
            Some((
                MethodHead::Operator(Operator::from_expression(node)?),
                vec![operand],
            ))
        } else {
            None
        }
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

    fn declared_type_parent(
        &self,
        value: SyntaxNode<'_>,
        type_name: Option<&ObjectName>,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<TypeId> {
        if let Some(parent) = ring::RingSemantics::value_parent(type_name, knowledge) {
            return TypeChecker::new(self, knowledge)
                .resolve_type_id_at(&parent, position, knowledge);
        }
        let type_name = type_name.filter(|type_name| {
            value.is::<NewStatement>()
                && TypeChecker::new(self, knowledge).is_subtype(
                    type_name,
                    &TypeRole::Type.object_name(),
                    position,
                    knowledge,
                )
        })?;
        let parent = new_statement_parent(value)
            .and_then(symbol_node_text)
            .map(ObjectName::new)
            .unwrap_or_else(|| type_name.clone());
        TypeChecker::new(self, knowledge).resolve_type_id_at(&parent, position, knowledge)
    }

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

    /// The options of a catalog method function, read from its runtime class.
    /// A record without a method function class leaves them unrecorded.
    fn catalog_method_options(
        record: &Record,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> MethodOptions {
        MethodOptions::of_runtime_class(&record.class, knowledge)
            .unwrap_or(MethodOptions::Unrecorded)
    }

    fn enrich_parameters(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        parameter_types: Option<&[ObjectName]>,
    ) {
        let parameter_nodes = nested_symbols(node, SyntaxNode::is_parameter_container);
        let typed_parameters = parameter_types.filter(|types| types.len() == parameter_nodes.len());
        for (index, parameter_node) in parameter_nodes.into_iter().enumerate() {
            self.enrich_symbol(
                parameter_node,
                source,
                BindingEnrichment {
                    presentation_kind: SymbolKind::VARIABLE,
                    inferred_type: typed_parameters
                        .and_then(|types| types.get(index))
                        .cloned()
                        .map(InferredType::upward_from_id),
                    object_id: None,
                    indexed_element_type: None,
                    parent_type: None,
                },
            );
        }
    }

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

    fn enrich_definitions(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        enrichment: BindingEnrichment,
    ) {
        let structured_target = !node.is::<Symbol>();
        for definition in nested_symbols(node, SyntaxNode::is_collection_expression) {
            let enrichment = if structured_target {
                BindingEnrichment {
                    inferred_type: None,
                    object_id: None,
                    ..enrichment.clone()
                }
            } else {
                enrichment.clone()
            };
            self.enrich_symbol(definition, source, enrichment);
        }
    }

    /// A `newPackage` name string binds its global symbol to the new package.
    fn enrich_package_declaration(
        &mut self,
        call: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let Some(binding_state) = declared_package_name(call)
            .and_then(|name| package_name_range(name, source))
            .and_then(|range| {
                self.registry
                    .binding_states_by_range
                    .get(&source_range_key(range))
                    .copied()
            })
        else {
            return;
        };
        self.enrich_binding_state(
            binding_state.binding,
            binding_state.state,
            BindingEnrichment {
                presentation_kind: SymbolKind::NAMESPACE,
                inferred_type: Some(InferredType::exact_from_id(TypeRole::Package.object_name())),
                object_id: None,
                indexed_element_type: None,
                parent_type: None,
            },
        );
    }

    fn enrich_symbol(
        &mut self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        enrichment: BindingEnrichment,
    ) {
        let Some(binding_state) = self
            .registry
            .binding_states_by_range
            .get(&source_range_key(source.range_for_node(node)))
            .copied()
        else {
            return;
        };
        self.enrich_binding_state(binding_state.binding, binding_state.state, enrichment);
    }

    fn enrich_binding_state(
        &mut self,
        binding_id: BindingId,
        state_index: usize,
        enrichment: BindingEnrichment,
    ) {
        let source_type = enrichment.parent_type.map(|parent_type| {
            let type_id = TypeId::from_source_type(
                ObjectId::new(format!("$Source${}:{state_index}", binding_id.0)),
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
        state.source_type = source_type;
    }

    fn add_binding(&mut self, fact: BindingFact) -> BindingId {
        let name = fact.name.clone();
        let binding_id = BindingId(self.registry.bindings.len() as u32);
        let range = fact.target;
        let state = BindingStateInfo {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: None,
            object_id: None,
            indexed_element_type: None,
            is_ring_generator: false,
            source_type: None,
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

    fn add_binding_state(&mut self, binding_id: BindingId, fact: BindingFact) -> Option<usize> {
        let state_index = self
            .binding(binding_id)
            .map(|binding| binding.states.len())?;
        let state = BindingStateInfo {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: None,
            object_id: None,
            indexed_element_type: None,
            is_ring_generator: false,
            source_type: None,
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

    pub fn local_method_installation_signature_at<'a>(
        &'a self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<(&'a FunctionInfo, &'a MethodInstallation)> {
        let assignment = method_installation_assignment_for_callable_node(node)?;
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

    pub fn is_method_installation_callable(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> bool {
        method_installation_assignment_for_callable_node(node)
            .and_then(|assignment| self.installation_for(assignment, source))
            .is_some()
    }

    pub fn infer_call_static_facts(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        let scope_idx = self
            .find_scope_at(source.position_for_node(node))
            .unwrap_or(0);
        TypeChecker::new(self, knowledge).infer_call_facts(node, source, scope_idx, knowledge)
    }

    pub fn local_call_installations<'a>(
        &'a self,
        function: &'a FunctionInfo,
        argument: SyntaxNode,
        position: Position,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<&'a MethodInstallation> {
        let facts = self.infer_call_static_facts(argument, source, knowledge);
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

    pub fn infer_expression_static_type(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        self.infer_expression_type(node, source, knowledge)
            .single()
            .cloned()
    }

    pub fn infer_expression_type(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let scope_idx = self
            .find_scope_at(source.position_for_node(node))
            .unwrap_or(0);
        TypeChecker::new(self, knowledge).type_of(node, source, scope_idx)
    }

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

    pub fn method_codomain_deduction(
        &self,
        assignment: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodCodomainDeduction> {
        let installation = self.installation_for(assignment, source)?;
        let (left, _, right) = assignment_parts(assignment)?;
        // The annotation precedes the installed lambda, while the codomain is
        // the value of the lambda receiving the arguments.
        let installed = InstalledFunction::of(right)?;
        let codomain = self.deducible_codomain(
            installation.options.argument_lambda(installed)?,
            source.position_for_node(assignment),
            source,
            knowledge,
        )?;

        let annotation = method_codomain_annotation(right);
        // Without a local annotation the installation inherits the method's
        // `TypicalValue`; annotating that same type again adds nothing. A
        // codomain only deduced is still worth writing down.
        if annotation.is_none()
            && installation.codomain_origin == Some(CodomainOrigin::TypicalValue)
            && installation.method.codomain.as_ref() == Some(&codomain)
        {
            return None;
        }
        let annotated_codomain = annotation.map(|annotation| ObjectName::new(annotation.text()));
        if let Some(annotation) = annotation {
            match TypeChecker::new(self, knowledge).subtype_evidence(
                &codomain,
                annotated_codomain.as_ref()?,
                source.position_for_node(annotation),
                knowledge,
            ) {
                SubtypeEvidence::Proven | SubtypeEvidence::Unknown => return None,
                SubtypeEvidence::Disproven => {}
            }
        }

        let edit = annotation.map_or_else(
            || {
                MethodCodomainEdit::Add({
                    let start = source.position_for_node(installed.node());
                    TextRange::new(start, start)
                })
            },
            |_| MethodCodomainEdit::Replace,
        );
        Some(MethodCodomainDeduction {
            codomain,
            annotated_codomain,
            diagnostic_range: annotation.map_or_else(
                || {
                    source.range_for_bytes(
                        left.start_byte()
                            ..assignment
                                .method_installation_head()
                                .map_or(left.trimmed_end_byte(), |head| {
                                    head.right.trimmed_end_byte()
                                }),
                    )
                },
                |node| source.range_for_node(node),
            ),
            edit,
        })
    }

    fn callable_object_for_value(
        &mut self,
        value: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallableObjectId> {
        if value.is_holder() {
            return self.callable_object_for_value(
                value.final_value_child()?,
                source,
                scope_idx,
                knowledge,
            );
        }
        if value.is::<Symbol>() {
            if let Some(binding) = self.get_binding_from_scope(
                value.text(),
                scope_idx,
                source.position_for_node(value),
            ) {
                if binding.scope_idx != 0
                    || !knowledge.shadows_source(&binding.name, binding.state.span.start)
                {
                    return binding.state.object_id;
                }
            }

            let indexed_id = knowledge.resolve_object(&ObjectName::new(value.text()))?;
            if let Some(object_id) = self
                .registry
                .indexed_callable_objects
                .get(&indexed_id)
                .copied()
            {
                return Some(object_id);
            }
            let record = knowledge.object(&indexed_id)?;
            let callable = record.callable()?;
            let function = FunctionInfo {
                typical_value: callable
                    .typical_value
                    .as_ref()
                    .and_then(|type_id| knowledge.type_name(type_id))
                    .cloned(),
                installations: Vec::new(),
                dispatch: None,
                runtime_class: [
                    TypeRole::MethodFunctionSingle,
                    TypeRole::MethodFunctionBinary,
                ]
                .into_iter()
                .find(|role| knowledge.has_type_role(&record.class, *role)),
                indexed_callable: Some(record.name.clone()),
                kind: match callable.kind {
                    CallableKind::MethodFunction => LocalFunctionKind::Method(
                        Self::catalog_method_options(knowledge.object(&indexed_id)?, knowledge),
                    ),
                    CallableKind::Function | CallableKind::Operator(_) => LocalFunctionKind::Plain,
                },
            };
            let object_id = CallableObjectId(self.registry.callable_objects.len());
            self.registry.callable_objects.push(function);
            self.registry
                .indexed_callable_objects
                .insert(indexed_id, object_id);
            return Some(object_id);
        }

        let function = if let Some(declaration) = MethodDeclaration::of(value) {
            Some(FunctionInfo {
                typical_value: declaration.typical_value(),
                installations: Vec::new(),
                dispatch: None,
                runtime_class: Some(declaration.runtime_class()),
                indexed_callable: None,
                kind: LocalFunctionKind::Method(declaration.options()),
            })
        } else if value.is::<LambdaExpression>() {
            Some(FunctionInfo {
                typical_value: None,
                installations: Vec::new(),
                runtime_class: None,
                indexed_callable: None,
                dispatch: self.registry.scopes.function_dispatch(value, source),
                kind: LocalFunctionKind::Plain,
            })
        } else {
            None
        };
        let function = function?;
        let id = CallableObjectId(self.registry.callable_objects.len());
        self.registry.callable_objects.push(function);
        Some(id)
    }

    /// Characterize an assignment once, retain its source fact, and attach that
    /// fact to the local callable registry when the installation takes effect.
    fn record_method_installation(
        &mut self,
        assignment: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let id = MethodInstallationId(self.registry.installations.len());
        let classified = self.classify_installation(id, assignment, source, knowledge)?;

        // Preserve M2's distinct assignment-method form: only `:=` contributes
        // a callable signature here. `=` installations are retained for
        // diagnostics/document symbols but are not ordinary call methods.
        self.retain_method_installation(
            classified,
            assignment_parts(assignment)
                .is_some_and(|(_, operator, _)| matches_token::<Token![:=]>(operator.text())),
            source,
            knowledge,
        )
    }

    fn record_install_method_call(
        &mut self,
        call: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let id = MethodInstallationId(self.registry.installations.len());
        let classified = self.classify_install_method(id, call, source, knowledge)?;
        self.retain_method_installation(classified, true, source, knowledge)
    }

    fn retain_method_installation(
        &mut self,
        classified: ClassifiedInstallation,
        attach: bool,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<MethodInstallationId> {
        let ClassifiedInstallation {
            mut installation,
            method_type_spans,
            installed,
        } = classified;
        if attach {
            installation.effect = self.attach_method_installation(&mut installation, knowledge);
        }
        installation.installed = installed.map_or(InstalledValue::Unknown, |installed| {
            self.installed_value(installed, installation.span.start, knowledge)
        });
        // Read once here so the arity check and parameter typing need not
        // re-walk the tree. Only a lambda carries a checkable arity.
        installation.installed_lambda_dispatch = match installed {
            Some(InstalledFunction::Lambda(lambda)) => {
                self.registry.scopes.function_dispatch(lambda, source)
            }
            _ => None,
        };
        let argument_lambda =
            installed.and_then(|installed| installation.options.argument_lambda(installed));
        installation.argument_lambda_dispatch = argument_lambda
            .and_then(|lambda| self.registry.scopes.function_dispatch(lambda, source));
        if let (MethodHead::Function(_), Some(lambda)) =
            (&installation.method.head, argument_lambda)
        {
            self.registry.installations_by_argument_lambda.insert(
                source_range_key(source.range_for_node(lambda)),
                installation.id,
            );
        }

        for (range, role) in method_type_spans {
            self.set_source_semantic_role(range, role);
        }
        let id = installation.id;
        debug_assert_eq!(installation.id.0, self.registry.installations.len());
        let position = installation.span.start;
        self.registry.installations.push(installation);
        if let Some(lambda) = argument_lambda {
            self.deduce_installation_codomain(id, lambda, position, source, knowledge);
        }
        Some(id)
    }

    /// Take the type the lambda `argument_lambda` of the installation `id`,
    /// at `position`, returns as its codomain when nothing states one: calls
    /// of the method then return it, as they would were it annotated.
    fn deduce_installation_codomain(
        &mut self,
        id: MethodInstallationId,
        argument_lambda: SyntaxNode,
        position: Position,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if self.registry.installations[id.0].method.codomain.is_some() {
            return;
        }
        let Some(codomain) = self.deducible_codomain(argument_lambda, position, source, knowledge)
        else {
            return;
        };
        let installation = &mut self.registry.installations[id.0];
        installation.method.codomain = Some(codomain);
        installation.codomain_origin = Some(CodomainOrigin::Deduction);
    }

    /// The type the lambda `argument_lambda`, installed at `position`,
    /// always returns, when inference knows it and an annotation at the
    /// installation could name it: a type named by a value of the lambda's
    /// own scope, such as `T` in `(T, x) -> new T from x`, exists only when
    /// the method runs.
    fn deducible_codomain(
        &self,
        argument_lambda: SyntaxNode,
        position: Position,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        let body = argument_lambda.lambda_body()?;
        let codomain = self.infer_expression_static_type(body, source, knowledge)?;
        if codomain == TypeRole::Thing.object_name() {
            return None;
        }
        (self.binding_id_at(codomain.name(), position)
            == self.binding_id_at(codomain.name(), source.position_for_node(body)))
        .then_some(codomain)
    }

    fn installed_value(
        &self,
        installed: InstalledFunction,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InstalledValue {
        match installed {
            InstalledFunction::Lambda(_) => InstalledValue::Lambda,
            InstalledFunction::OptionDeclaring { .. } => InstalledValue::OptionDeclaring,
            InstalledFunction::Other(value) => {
                let library_record = symbol_node_text(value)
                    .filter(|name| self.get_binding_at(name, position).is_none())
                    .and_then(|name| knowledge.get_record(&ObjectName::new(name)))
                    .and_then(|record| Some((record, record.callable()?)));
                match library_record {
                    Some((record, callable)) => InstalledValue::LibraryFunction {
                        declares_options: !callable.options.is_empty()
                            || MethodOptions::of_runtime_class(&record.class, knowledge)
                                .is_some_and(|options| options != MethodOptions::Absent),
                    },
                    None => InstalledValue::Unknown,
                }
            }
        }
    }

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
                    });
                function.installations.push(installation.id);
                MethodInstallationEffect::Effective
            }
        }
    }

    pub fn binding_id_at(&self, name: &str, pos: Position) -> Option<BindingId> {
        let scope_idx = self.find_scope_at(pos)?;
        self.binding_id_from_scope(name, scope_idx, pos)
    }
}

struct TypedSemanticRoles<'analysis, 'tree, 'source, 'knowledge, Source: ?Sized, Knowledge: ?Sized>
{
    analysis: &'analysis mut Analysis,
    root: SyntaxNode<'tree>,
    source: &'source Source,
    knowledge_provider: &'knowledge Knowledge,
}

impl<'tree, Source, Knowledge> TypedSemanticRoles<'_, 'tree, '_, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn register_option(&mut self, node: &OptionExpression) {
        let Some(expression) = self.root.related(node) else {
            return;
        };
        let Some(left) = expression.binary_parts().map(|parts| parts.left) else {
            return;
        };
        self.analysis.register_option_key(left, self.source);
    }

    fn register_property(&mut self, node: &(impl crate::node_metadata::SyntaxType + ?Sized)) {
        let Some(expression) = self.root.related(node) else {
            return;
        };
        if let Some(property) = expression.property_key() {
            self.analysis.register_source_semantic_role(
                self.source.range_for_node(property),
                SourceSemanticRole::PropertyKey,
            );
        }
    }

    fn register_namespace<Syntax>(&mut self, syntax: &Syntax)
    where
        Syntax: crate::node_metadata::SyntaxType,
    {
        let Some(node) = self.root.related(syntax) else {
            return;
        };
        let knowledge = self
            .knowledge_provider
            .at_position(self.source.position_for_node(node));
        self.analysis
            .record_namespace_role(node, self.source, &knowledge);
    }
}

impl<'ast, Source, Knowledge> Visit<'ast> for TypedSemanticRoles<'_, '_, '_, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr_option(&mut self, node: &'ast OptionExpression) {
        self.register_option(node);
        visit::visit_expr_option(self, node);
    }

    fn visit_expr_op_bin_regular(&mut self, node: &'ast BinaryExpr) {
        self.register_property(node);
        visit::visit_expr_op_bin_regular(self, node);
    }

    fn visit_expr_op_bin_member(&mut self, node: &'ast ExprOpBinMember) {
        self.register_property(node);
        visit::visit_expr_op_bin_member(self, node);
    }

    fn visit_expr_op_bin_index(&mut self, node: &'ast ExprOpBinIndex) {
        self.register_property(node);
        visit::visit_expr_op_bin_index(self, node);
    }

    fn visit_expr_quote(&mut self, node: &'ast QuoteExpression) {
        if let Some(quote) = self.root.related(node) {
            self.analysis.record_quoted_symbol_role(quote, self.source);
        }
        visit::visit_expr_quote(self, node);
    }

    fn visit_string_literal(&mut self, node: &'ast StringLiteral) {
        self.register_namespace(node);
        visit::visit_string_literal(self, node);
    }

    fn visit_raw_string_literal(&mut self, node: &'ast RawStringLiteral) {
        self.register_namespace(node);
        visit::visit_raw_string_literal(self, node);
    }
}

pub(crate) fn assigned_lambda(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let node = parenthesized_value(node)?;
    if node.is::<LambdaExpression>() {
        return Some(node);
    }
    node.has_binary_operator::<Token![=>]>()
        .then(|| node.binary_parts().map(|parts| parts.right))
        .flatten()
        .and_then(assigned_lambda)
}

pub(crate) fn method_codomain_annotation(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if let Some(parent) = node.parent() {
        let annotation = parent
            .get::<m2_syn::nodes::ExprAssignInstallMethod>()
            .and_then(|node| node.typical_value.as_ref())
            .or_else(|| {
                parent
                    .get::<m2_syn::nodes::ExprAssignInstallOp>()
                    .and_then(|node| node.typical_value.as_ref())
            });
        if let Some(annotation) = annotation {
            return parent.related(annotation.typical_value.as_ref());
        }
    }

    node.is_option_assignment()
        .then(|| node.binary_parts().map(|parts| parts.left))
        .flatten()
}

fn single_symbol_assignment_target<'tree>(node: SyntaxNode<'tree>) -> Option<&'tree str> {
    node.is::<Symbol>().then(|| node.text())
}

pub fn symbol_node_text<'tree>(node: SyntaxNode<'tree>) -> Option<&'tree str> {
    node.is_symbol_like().then(|| node.text())
}

fn is_expression_symbol(node: SyntaxNode<'_>) -> bool {
    if !node.is::<Symbol>() || matches!(node.text(), "true" | "false") {
        return false;
    }
    if node
        .parent()
        .is_some_and(|parent| parent.is::<QuoteExpression>())
    {
        return false;
    }

    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.is_assignment() {
            return parent
                .binary_parts()
                .map(|parts| parts.left)
                .is_none_or(|left| !left.contains(node));
        }
        current = parent;
    }

    true
}

fn is_condition_value(node: SyntaxNode<'_>) -> bool {
    if !node.is::<Symbol>() {
        return false;
    }

    if node.parent().is_some_and(|parent| {
        parent.is_space_application()
            && parent
                .binary_parts()
                .map(|parts| parts.left)
                .is_some_and(|left| left.id() == node.id())
    }) {
        return false;
    }

    let mut condition = node;
    while let Some(parent) = condition.parent() {
        let owner_condition = (parent.is::<IfStatement>() || parent.is::<WhileLoop>())
            .then(|| parent.control_condition())
            .flatten();
        if let Some(owner_condition) = owner_condition {
            return owner_condition.id() == condition.id();
        }
        condition = parent;
    }

    false
}

/// The options of Macaulay2's `method` that shape the method function it creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MethodDeclarationOption {
    Binary,
    Dispatch,
    Options,
    TypicalValue,
}

impl MethodDeclarationOption {
    fn name(self) -> &'static str {
        match self {
            Self::Binary => "Binary",
            Self::Dispatch => "Dispatch",
            Self::Options => "Options",
            Self::TypicalValue => "TypicalValue",
        }
    }
}

/// A `method(...)` call, which creates a method function.
///
/// Its options are read from the call's own arguments only, so an option list
/// such as `Options => {Binary => true}` does not configure the method itself.
#[derive(Debug, Clone, Copy)]
struct MethodDeclaration<'tree> {
    call: SyntaxNode<'tree>,
}

impl<'tree> MethodDeclaration<'tree> {
    fn of(node: SyntaxNode<'tree>) -> Option<Self> {
        (node.is_space_application()
            && node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(symbol_node_text)
                == Some("method"))
        .then_some(Self { call: node })
    }

    fn option(self, option: MethodDeclarationOption) -> Option<SyntaxNode<'tree>> {
        let arguments = parenthesized_value(self.call.binary_parts().map(|parts| parts.right)?)?;
        let mut arguments = if arguments.is_sequence() {
            arguments.collection_elements().collect::<Vec<_>>()
        } else {
            vec![arguments]
        };
        arguments.retain(|argument| {
            argument.is_option_assignment()
                && argument
                    .binary_parts()
                    .map(|parts| parts.left)
                    .and_then(symbol_node_text)
                    == Some(option.name())
        });
        arguments.pop()?.binary_parts().map(|parts| parts.right)
    }

    fn typical_value(self) -> Option<ObjectName> {
        self.option(MethodDeclarationOption::TypicalValue)
            .and_then(symbol_node_text)
            .map(ObjectName::new)
    }

    /// `method` takes options exactly when `Options` is given and not `null`;
    /// `true` forwards them, any other value (an empty list and `false`
    /// included) passes them as a table.
    fn options(self) -> MethodOptions {
        match self.option(MethodDeclarationOption::Options) {
            None => MethodOptions::Absent,
            Some(options) if options.is_null_symbol() => MethodOptions::Absent,
            Some(options) if symbol_node_text(options) == Some("true") => MethodOptions::Forwarded,
            Some(_) => MethodOptions::Table,
        }
    }

    /// The runtime class `method` picks: `Binary => true` first, then a single
    /// `Dispatch` of `Thing` or `Type`, then whether there are options.
    fn runtime_class(self) -> TypeRole {
        let is_symbol = |option, role: TypeRole| {
            self.option(option).and_then(symbol_node_text) == Some(role.object_name().name())
        };
        if self
            .option(MethodDeclarationOption::Binary)
            .and_then(symbol_node_text)
            == Some("true")
        {
            TypeRole::MethodFunctionBinary
        } else if is_symbol(MethodDeclarationOption::Dispatch, TypeRole::Thing)
            || is_symbol(MethodDeclarationOption::Dispatch, TypeRole::Type)
        {
            TypeRole::MethodFunctionSingle
        } else if self.options() != MethodOptions::Absent {
            TypeRole::MethodFunctionWithOptions
        } else {
            TypeRole::MethodFunction
        }
    }
}

fn literal_option_assignment(node: SyntaxNode) -> Option<LiteralOption> {
    if !node.is_option_assignment() {
        return None;
    }

    let left = node.binary_parts().map(|parts| parts.left)?;
    let right = node.binary_parts().map(|parts| parts.right)?;
    let key = symbol_node_text(left)?;
    let value = literal_option_value(right)?;
    Some(LiteralOption {
        option: ObjectName::new(key),
        value: ObjectName::new(value),
    })
}

fn literal_option_value(node: SyntaxNode<'_>) -> Option<&str> {
    if symbol_node_text(node).is_some()
        || node.is::<IntegerLiteral>()
        || node.is::<FloatLiteral>()
        || node.is::<StringLiteral>()
        || node.is::<RawStringLiteral>()
    {
        Some(node.text())
    } else {
        None
    }
}

fn method_installation_assignment_for_callable_node<'tree>(
    node: SyntaxNode<'tree>,
) -> Option<SyntaxNode<'tree>> {
    if let Some(parent) = node.parent() {
        if let Some(installation) = parent.get::<m2_syn::nodes::ExprAssignInstallMethod>() {
            if parent
                .related(installation.method.as_ref())
                .is_some_and(|method| method.contains(node))
            {
                return Some(parent);
            }
        }
    }

    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;

        if !current.is_space_application() {
            continue;
        }

        let callable = current.binary_parts().map(|parts| parts.left)?;
        if !callable.contains(node) {
            continue;
        }

        if is_colon_equal_assignment_left(current) {
            return current.parent();
        }
    }

    None
}

/// The first direct clause of `node` of the given kind (`then`/`else`/`do`/…).
pub(crate) fn clause_of<'tree, Syntax: SyntaxKind>(
    node: SyntaxNode<'tree>,
) -> Option<SyntaxNode<'tree>> {
    node.named_children().find(|child| child.is::<Syntax>())
}

/// The value expression a clause wraps (`then E` → `E`): its single named child.
pub(crate) fn clause_value(clause: SyntaxNode) -> Option<SyntaxNode> {
    clause
        .clause_value()
        .or_else(|| clause.named_children().next())
}

fn new_statement_parent(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    new_statement_value(clause_value(node.new_parent_clause()?)?)
}

fn new_statement_instance(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    new_statement_value(clause_value(node.new_instance_clause()?)?)
}

fn new_statement_value(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if node.is_assignment() {
        node.binary_parts().map(|parts| parts.left)
    } else {
        Some(node)
    }
}

fn new_statement_installation_assignment(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    node.is::<NewStatement>()
        .then(|| {
            [
                node.new_class(),
                node.new_parent_clause(),
                node.new_instance_clause(),
            ]
            .into_iter()
            .flatten()
            .find(|value| value.is_assignment())
        })
        .flatten()
}

pub(crate) fn assignment_parts(
    node: SyntaxNode<'_>,
) -> Option<(SyntaxNode<'_>, SyntaxNode<'_>, SyntaxNode<'_>)> {
    if node.is::<NewStatement>() {
        let assignment = new_statement_installation_assignment(node)?;
        return Some((
            node,
            assignment.operator_node()?,
            assignment.binary_parts().map(|parts| parts.right)?,
        ));
    }
    Some((
        node.binary_parts().map(|parts| parts.left)?,
        node.operator_node()?,
        node.binary_parts().map(|parts| parts.right)?,
    ))
}

fn direct_callback_callable(lambda: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut argument = lambda;
    let mut parent = argument.parent()?;
    while parent.is::<Sequence>() || parent.is_holder() {
        if parent.is::<Sequence>()
            && parent
                .collection_elements()
                .last()
                .is_none_or(|last| last.id() != argument.id())
        {
            return None;
        }
        argument = parent;
        parent = argument.parent()?;
    }

    if !parent.is_space_application()
        || parent
            .binary_parts()
            .map(|parts| parts.right)
            .is_none_or(|right| right.id() != argument.id())
    {
        return None;
    }
    parent
        .binary_parts()
        .map(|parts| parts.left)
        .filter(|callable| callable.is::<Symbol>())
}

/// The value a node denotes, peeling parenthesized grouping: `(a)` → `a`,
/// `((a))` → `a`. A parenthesized expression whose final child is `muted`
/// (`(a;)`) denotes null, so it has no value node. A non-parenthesized node is
/// its own value. `()` and `(a, b)` are `Sequence` nodes, left untouched.
pub(crate) fn parenthesized_value(node: SyntaxNode) -> Option<SyntaxNode> {
    let mut current = node;
    while current.is_holder() {
        current = current.final_value_child()?;
    }
    Some(current)
}

fn method_installation_domain_nodes(node: SyntaxNode) -> Option<Vec<SyntaxNode>> {
    let node = parenthesized_value(node)?;
    if node.is::<Sequence>() || node.is::<List>() {
        let domain = node.collection_elements().collect::<Vec<_>>();
        return (!domain.is_empty()).then_some(domain);
    }

    symbol_node_text(node).map(|_| vec![node])
}

fn call_like_left_symbol_for_argument<'tree>(
    mut node: SyntaxNode<'tree>,
    allow_list_argument: bool,
) -> Option<&'tree str> {
    let mut parent = node.parent()?;
    if parent.is::<Sequence>() && !parent.is_first_collection_element(node) {
        return None;
    }

    loop {
        if parent.is_space_application() {
            let left = parent.binary_parts().map(|parts| parts.left)?;
            if left.is::<Symbol>() {
                return Some(left.text());
            }
        }

        if parent.is::<List>() && !allow_list_argument {
            return None;
        }
        if !parent.is::<Sequence>() && !parent.is::<List>() && !parent.is_holder() {
            return None;
        }

        node = parent;
        parent = node.parent()?;
    }
}

fn indexed_string_names_package(
    node: SyntaxNode<'_>,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> bool {
    let Some(callable_name) = call_like_left_symbol_for_argument(node, false) else {
        return false;
    };
    let Some(callable) = knowledge
        .get_record(&ObjectName::new(callable_name))
        .and_then(|record| record.callable())
    else {
        return false;
    };
    let (Some(string), Some(package)) = (
        knowledge.type_role_id(TypeRole::String),
        knowledge.type_role_id(TypeRole::Package),
    ) else {
        return false;
    };
    let accepts_string = callable
        .methods
        .iter()
        .any(|method| method.domain.first() == Some(string.object()));
    let accepts_package = callable
        .methods
        .iter()
        .any(|method| method.domain.first() == Some(package.object()));
    let returns_package = callable.methods.iter().any(|method| {
        method.domain.first() == Some(string.object()) && method.codomain.as_ref() == Some(&package)
    });
    accepts_string && (accepts_package || returns_package)
}

fn is_colon_equal_assignment_left(node: SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if !parent.is_assignment() {
        return false;
    }
    if parent
        .binary_parts()
        .map(|parts| parts.left)
        .is_none_or(|left| left.id() != node.id())
    {
        return false;
    }

    parent
        .operator_node()
        .is_some_and(|operator| matches_token::<Token![:=]>(operator.text()))
}
