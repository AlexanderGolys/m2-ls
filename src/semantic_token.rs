//! Semantic-token classification over inferred and indexed object facts.
//!
//! A name is colored by the runtime class of the object it is bound to — the
//! class the library index records, or the static type the analysis infers
//! for a local — not by its spelling or by how it is called. Macaulay2 does
//! not separate classes from types, so every `Type` object (`ZZ`, `Matrix`,
//! a `new Type`) is sent as [`Type`](M2SemanticTokenType::Type).
//!
//! # Functions and methods
//!
//! In Macaulay2 a *method* is one installation of a function on a domain:
//! `det Matrix := …` installs the method `det(Matrix)`, and `det m` runs the
//! body installed for the class of `m`. The object that owns installations
//! and dispatches to them is a *method function*, made by `method()`. The
//! [`Method`](M2SemanticTokenType::Method) token marks a method function;
//! everything else applied to arguments is a
//! [`Function`](M2SemanticTokenType::Function):
//!
//! | Runtime class | Example | Token |
//! | --- | --- | --- |
//! | `MethodFunction`, `MethodFunctionSingle`, `MethodFunctionBinary`, `MethodFunctionWithOptions` | `source`, `flatten`, `gcd`, `det` | `method` |
//! | `FunctionClosure` | `x -> x + 1`, `print` | `function` |
//! | `CompiledFunction` | `apply`, `scan` | `function` + `defaultLibrary` by default |
//! | `CompiledFunctionClosure` | | `function` |
//! | `ScriptedFunctor` | `id`, `Ext`, `HH` | `function` |
//! | `Command`, `Manipulator` | `exit`, `help`; `close`, `endl` | `function` |
//!
//! The class decides, not the signatures the documentation lists: the index
//! records typed signatures for many compiled functions (`apply`, `keys`),
//! but those are documentation stubs, not installations, and `apply` stays a
//! function. A scripted functor (`Ext^1(M, N)`, `id_M`) dispatches through
//! its own subscript, superscript and argument functions rather than through
//! installations, so it too is a function. A command is not a function at
//! all — `Command` and `Manipulator` are lists wrapping one — but it reads as
//! one: a command runs when evaluated alone (`exit`), a manipulator when
//! printed to a file (`stdio << endl`).
//!
//! The installation head, the `det` of `det Matrix := …`, keeps its own
//! token and adds [`Declaration`](M2SemanticTokenModifier::Declaration); the
//! domain `Matrix` is a [`TypeAnnotation`](M2SemanticTokenType::TypeAnnotation).
//!
//! `semanticTokens.defaultLibrary` selects which objects receive that modifier.
//! By default it marks `CompiledFunction` values. `Compiled` also includes
//! `CompiledFunctionClosure` subtypes, such as method dispatchers. Package
//! policies instead select the package owning the binding.
//! `readonly` denotes protection of the binding, not mutability of its value.

use crate::builtin_index::Record;
use crate::meta::{BindingRole, Metadata};
use crate::node_metadata::SyntaxNode;
use crate::object_registry::{ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView};
use crate::settings::SemanticTokenSettings;
use crate::source::DocumentSpan;
use crate::typesystem::TypeKnowledge;
use m2_syn::nodes::{FloatLiteral, IntegerLiteral};
use strum::{EnumIter, IntoEnumIterator};
use these_macros_should_be_illegal::enum_fn;
use tower_lsp::lsp_types::{
    SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend, SymbolKind,
};
/// The runtime classes of method functions. They are unrelated siblings —
/// `MethodFunction` descends from `CompiledFunctionClosure`, the others from
/// `FunctionClosure` — so no common ancestor identifies a method function.
const METHOD_FUNCTION_CLASSES: [&str; 4] = [
    "MethodFunction",
    "MethodFunctionSingle",
    "MethodFunctionBinary",
    "MethodFunctionWithOptions",
];

/// The runtime classes of objects that act without being applied: a
/// `Command` when evaluated alone, a `Manipulator` when printed to a file.
const COMMAND_CLASSES: [&str; 2] = ["Command", "Manipulator"];

/// Indexed facts needed specifically for semantic-token classification.
///
/// This is separate from [`TypeKnowledge`] because syntax/type analysis should
/// not depend on editor presentation roles. Both the concrete corpus and a
/// document-scoped package view implement it, so semantic highlighting follows
/// the same import resolution order as the other language features.
pub trait SemanticTokenKnowledge: TypeKnowledge {
    /// Classify an indexed object by name.
    fn semantic_token(&self, name: &str) -> Option<M2SemanticToken>;

    /// Classify a local object from its inferred static type.
    fn semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken>;

    /// Whether `name` is a protected object whose class is exactly `Symbol`.
    fn is_protected_symbol(&self, name: &str) -> bool;

    /// Whether the indexed binding of `name` is protected from assignment.
    fn is_protected_global(&self, name: &str) -> bool;
}

/// The token types, named in Macaulay2 terms and each sent as a standard
/// LSP type. Each discriminant is its legend index; finer distinctions are
/// carried by [modifiers](M2SemanticTokenModifier). The module documentation
/// says which objects are functions and which are methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
#[enum_fn(pub const lsp_type -> SemanticTokenType)]
#[non_exhaustive]
#[repr(u32)]
pub enum M2SemanticTokenType {
    /// A type: any object whose class is a `Type`, such as `ZZ`, `Matrix`
    /// or a type a package defines.
    Type = SemanticTokenType::TYPE,
    /// An object applied to arguments that is not a method function: a
    /// function closure, a compiled function, a scripted functor, or a
    /// command or manipulator.
    Function = SemanticTokenType::FUNCTION,
    /// Any other value: a local, a global, or a library object such as
    /// `stdio`.
    Variable = SemanticTokenType::VARIABLE,
    /// A parameter of a function, or an input a documentation page names.
    Parameter = SemanticTokenType::PARAMETER,
    /// A key of an object rather than a binding of its own, such as an
    /// option name that is not a protected symbol.
    Property = SemanticTokenType::PROPERTY,
    /// A package.
    Namespace = SemanticTokenType::NAMESPACE,
    /// A name used as a `Symbol` rather than for a value: a protected symbol
    /// (`Strategy`, an option value such as `Linear`) or a quoted name
    /// (`symbol f`). Sent as `enumMember`, the standard type closest to it.
    Symbol = SemanticTokenType::ENUM_MEMBER,
    Keyword = SemanticTokenType::KEYWORD,
    String = SemanticTokenType::STRING,
    Number = SemanticTokenType::NUMBER,
    Operator = SemanticTokenType::OPERATOR,
    Comment = SemanticTokenType::COMMENT,
    /// A method function: an object made by `method()` that dispatches to
    /// the methods installed on it, such as `det` or `source`.
    Method = SemanticTokenType::METHOD,
    /// The modifier keywords `local`, `global`, `symbol` and their kin.
    QuoteModifier = SemanticTokenType::MODIFIER,
    /// A domain or codomain of an installation: the `ZZ` and `List` of
    /// `f ZZ := List => …`. Sent as `typeParameter`.
    TypeAnnotation = SemanticTokenType::TYPE_PARAMETER,
    /// Markup around documentation text: a TeX delimiter, the `{\tt` and
    /// `}` of `{\tt x}`, or a hypertext constructor such as `TT` or `PARA`.
    /// The text it styles stays a [`String`](Self::String) carrying the
    /// style's modifier.
    Macro = SemanticTokenType::MACRO,
    /// A documentation annotation: a SimpleDoc section keyword such as
    /// `Key` or `Description`, or a link to another
    /// documentation page, `TO`, `TOH` or `TO2`. Sent as `decorator`.
    Annotation = SemanticTokenType::DECORATOR,
    Regexp = SemanticTokenType::REGEXP,
}

impl M2SemanticTokenType {
    /// Whether to emit this syntax token when the client advertises
    /// `semanticTokens.augmentsSyntaxTokens`, meaning it layers semantic tokens
    /// over syntax highlighting. Quote modifiers (`local`, `global`, `symbol`,
    /// etc.) still receive the LSP `modifier` classification; ordinary syntax
    /// tokens defer to the editor's highlighter. This flag does not tell the
    /// server whether a particular syntax grammar is installed.
    pub const fn emit_alongside_syntax_highlighting(self) -> bool {
        matches!(self, Self::QuoteModifier)
    }
}

/// The modifiers m2-ls attaches to tokens. Like [`M2SemanticTokenType`],
/// the legend lists the variants in declaration order, so a variant's
/// discriminant is its bit position. Some are standard LSP modifiers; the
/// rest are Macaulay2's own, and `standardOnly` drops them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
#[enum_fn(pub const lsp_modifier -> SemanticTokenModifier)]
#[non_exhaustive]
#[repr(u32)]
pub enum M2SemanticTokenModifier {
    /// An option key, such as `Strategy` in `Strategy => Linear`.
    Option = SemanticTokenModifier::new("option"),
    /// Where a name is bound — `x := 1`, a ring generator in `QQ[x]` — or
    /// what an installation installs on, the `f` of `f ZZ := …`.
    Declaration = SemanticTokenModifier::DECLARATION,
    /// A value selected by `semanticTokens.defaultLibrary`: by default a
    /// `CompiledFunction`, optionally a compiled closure or package global.
    DefaultLibrary = SemanticTokenModifier::DEFAULT_LIBRARY,

    /// A binding protected from reassignment, independently of value mutability.
    Readonly = SemanticTokenModifier::READONLY,
    /// Documentation: a documentation call with its tags and values, the
    /// string a `doc` call reads as SimpleDoc, an attached documentation
    /// comment, and the code they embed.
    Documentation = SemanticTokenModifier::DOCUMENTATION,
    /// A generator introduced by an algebraic constructor, such as the `x`
    /// of `QQ[x]`.
    Algebraic = SemanticTokenModifier::new("algebraic"),
    /// Bold documentation text: `{\bf …}`, `BOLD`, a heading.
    Strong = SemanticTokenModifier::new("strong"),
    /// Italic documentation text: `{\em …}`, `{\it …}`, `EM`, `ITALIC`.
    Emphasis = SemanticTokenModifier::new("emphasis"),
    /// Monospace documentation text: `{\tt …}`, `TT`, `CODE`, `PRE`, a
    /// SimpleDoc `Pre` block.
    Monospace = SemanticTokenModifier::new("monospace"),
    /// The address of a link: `\url{…}`, `HREF`.
    Link = SemanticTokenModifier::new("link"),
    /// TeX mathematics, `$…$`.
    Math = SemanticTokenModifier::new("math"),
    /// Code shown or executed as a documentation example.
    Example = SemanticTokenModifier::new("example"),
    /// Code in a `TEST` block.
    Test = SemanticTokenModifier::new("test"),
}

impl M2SemanticTokenModifier {
    /// The modifier's bit in an [`M2SemanticTokenModifiers`] set, which is
    /// also its bit on the wire.
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// Whether the modifier is one the LSP specification defines, and so
    /// is still sent under `standardOnly`.
    pub const fn is_standard(self) -> bool {
        matches!(
            self,
            Self::Declaration | Self::DefaultLibrary | Self::Readonly | Self::Documentation
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct M2SemanticTokenModifiers(u32);

impl M2SemanticTokenModifiers {
    pub const fn with(self, modifier: M2SemanticTokenModifier) -> Self {
        Self(self.0 | modifier.bit())
    }

    pub const fn contains(self, modifier: M2SemanticTokenModifier) -> bool {
        self.0 & modifier.bit() != 0
    }
}

/// Runtime implementation facts retained across local aliases and workspace
/// indexing so a configuration refresh can choose highlighting without
/// repeating type analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledCallableKind {
    Function,
    Closure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct M2SemanticToken {
    pub token_type: M2SemanticTokenType,
    pub modifiers: M2SemanticTokenModifiers,
    pub compiled_callable: Option<CompiledCallableKind>,
}

impl M2SemanticToken {
    pub const fn new(token_type: M2SemanticTokenType) -> Self {
        Self {
            token_type,
            modifiers: M2SemanticTokenModifiers(0),
            compiled_callable: None,
        }
    }

    /// The token a name written inside a string is emitted as, when this is
    /// the token of what it names. A plain variable has no highlight of its
    /// own — editors draw it in the default text colour, which would blank
    /// it out of the string around it — so it keeps the string's colour.
    pub fn within_string(self) -> Option<Self> {
        (self != Self::new(M2SemanticTokenType::Variable)).then_some(self)
    }

    /// The token of an option name written as a symbol: a protected
    /// symbol such as `Strategy` stays a [`Symbol`](M2SemanticTokenType::Symbol),
    /// any other name is a [`Property`](M2SemanticTokenType::Property) of the
    /// options it is a key of.
    pub fn option_name(name: &str, knowledge: &(impl SemanticTokenKnowledge + ?Sized)) -> Self {
        let token_type = if knowledge.is_protected_symbol(name) {
            M2SemanticTokenType::Symbol
        } else {
            M2SemanticTokenType::Property
        };
        Self::new(token_type)
            .with_modifier(M2SemanticTokenModifier::Option)
            .with_binding_protection(knowledge.is_protected_global(name))
    }

    pub const fn with_modifier(self, modifier: M2SemanticTokenModifier) -> Self {
        self.with_modifier_state(modifier, true)
    }

    /// Set or clear one modifier while preserving runtime classification facts.
    pub const fn with_modifier_state(
        mut self,
        modifier: M2SemanticTokenModifier,
        enabled: bool,
    ) -> Self {
        if enabled {
            self.modifiers = self.modifiers.with(modifier);
        } else {
            self.modifiers.0 &= !modifier.bit();
        }
        self
    }

    /// Set protection for the binding this occurrence names. This replaces any
    /// protection inherited while classifying another binding, such as the
    /// target of an exported synonym.
    pub const fn with_binding_protection(self, protected: bool) -> Self {
        self.with_modifier_state(M2SemanticTokenModifier::Readonly, protected)
    }

    /// The legend the `initialize` response states: every type and modifier.
    pub fn legend() -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: M2SemanticTokenType::iter()
                .map(|token_type| token_type.lsp_type())
                .collect(),
            token_modifiers: M2SemanticTokenModifier::iter()
                .map(|modifier| modifier.lsp_modifier())
                .collect(),
        }
    }

    /// The token as sent against [`legend`](Self::legend): its type index and
    /// modifier bitset, without the M2-specific modifiers under
    /// `standardOnly`.
    pub fn encode(self, settings: &SemanticTokenSettings) -> (u32, u32) {
        if !settings.standard_only {
            return (self.token_type as u32, self.modifiers.0);
        }
        let modifiers = M2SemanticTokenModifier::iter()
            .filter(|modifier| self.modifiers.contains(*modifier) && modifier.is_standard())
            .fold(
                M2SemanticTokenModifiers::default(),
                M2SemanticTokenModifiers::with,
            );
        (self.token_type as u32, modifiers.0)
    }
}

/// The syntactic role of one source occurrence, recorded by analysis when
/// its position changes how it should be highlighted. Unlike an inferred
/// runtime class, this describes the use of a name: `f` in `symbol f` is a
/// quoted symbol even if its binding holds a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSemanticRole {
    RingGenerator,
    /// A ring generator where a constructor declares it: the `x`, `y_1` and
    /// `z_0 .. z_2` of `QQ[x, y_1, z_0 .. z_2]`, quoted or not.
    RingGeneratorDeclaration,
    MethodTypeParameter,
    /// What an installation installs on, the `f` of `f ZZ := …` or the `+`
    /// of `T + T := …`: highlighted as it is elsewhere, marked as defining.
    InstallationHead,
    /// The codomain an installation annotates, the `List` of
    /// `f ZZ := List => x -> …`: Macaulay2's syntax for stating the
    /// installation's typical value.
    CodomainAnnotation,
    OptionKey,
    PropertyKey,
    NamespaceArgument,
    /// The name a quote yields as a `Symbol` rather than evaluating —
    /// `symbol f`, `global f`, `local f`, `threadVariable f`, `threadLocal f`.
    /// The quote wins over whatever `f` is otherwise bound to.
    QuotedSymbol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSemanticToken {
    pub span: DocumentSpan,
    pub syntax_token_type: Option<M2SemanticTokenType>,
    pub source_role: Option<SourceSemanticRole>,
    pub is_symbol: bool,
    pub is_unquoted_symbol: bool,
    pub is_expression_symbol: bool,
    pub is_condition_value: bool,
}

pub struct SourceSemanticTokenContext<'a, M: ?Sized> {
    pub source_text: &'a str,
    pub source_token: &'a SourceSemanticToken,
    pub binding: Option<&'a M>,
    pub is_declaration: bool,
    pub workspace_token: Option<M2SemanticToken>,
    pub emit_syntax: bool,
}

pub fn classify_source_semantic_token<M>(
    context: SourceSemanticTokenContext<'_, M>,
    knowledge: &(impl SemanticTokenKnowledge + ?Sized),
) -> Option<M2SemanticToken>
where
    M: Metadata + ?Sized,
{
    let indexed_token = context
        .source_token
        .is_symbol
        .then(|| knowledge.semantic_token(context.source_text))
        .flatten();

    let installs = context.source_token.source_role == Some(SourceSemanticRole::InstallationHead);
    let mut token = context
        .source_token
        .source_role
        .as_ref()
        .filter(|_| !installs)
        .and_then(|role| {
            source_role_semantic_token(
                role,
                context.source_token.is_unquoted_symbol,
                context.source_text,
                knowledge,
            )
        });

    if token.is_none() {
        token = context.binding.map(|binding| {
            let token = local_symbol_semantic_token(binding, knowledge);
            if context.is_declaration {
                token.with_modifier(M2SemanticTokenModifier::Declaration)
            } else {
                token
            }
        });
    }

    if context.is_declaration
        && token.is_some_and(|token| token.modifiers.contains(M2SemanticTokenModifier::Algebraic))
    {
        token = token.map(|token| token.with_modifier(M2SemanticTokenModifier::Declaration));
    }
    token = token.or(indexed_token);
    token = token.or(context.workspace_token);
    if token.is_none()
        && context.source_token.is_expression_symbol
        && context.source_token.syntax_token_type.is_none()
    {
        token = Some(M2SemanticToken::new(M2SemanticTokenType::Variable));
    }
    if token.is_none() && (context.emit_syntax || installs) {
        token = context
            .source_token
            .syntax_token_type
            .map(M2SemanticToken::new);
    }
    if installs {
        token = token.map(|token| token.with_modifier(M2SemanticTokenModifier::Declaration));
    }
    token
}

pub fn local_symbol_semantic_token(
    symbol: &(impl Metadata + ?Sized),
    knowledge: &(impl SemanticTokenKnowledge + ?Sized),
) -> M2SemanticToken {
    let meta = symbol.meta();
    if meta.binding_role == Some(BindingRole::Parameter) {
        return M2SemanticToken::new(M2SemanticTokenType::Parameter);
    }

    if let Some(token) = meta
        .type_label
        .as_deref()
        .and_then(|type_name| local_symbol_static_type_token(symbol, type_name, knowledge))
    {
        return token;
    }

    M2SemanticToken::new(match meta.symbol_kind {
        Some(SymbolKind::FUNCTION) => M2SemanticTokenType::Function,
        Some(SymbolKind::VARIABLE) => M2SemanticTokenType::Variable,
        Some(SymbolKind::METHOD) => M2SemanticTokenType::Method,
        Some(SymbolKind::CLASS) => M2SemanticTokenType::Type,
        Some(SymbolKind::NAMESPACE) => M2SemanticTokenType::Namespace,
        Some(SymbolKind::PROPERTY) => M2SemanticTokenType::Property,
        Some(SymbolKind::CONSTANT) => M2SemanticTokenType::Symbol,
        _ => M2SemanticTokenType::Variable,
    })
}

pub fn syntax_semantic_token_type(node: SyntaxNode<'_>) -> Option<M2SemanticTokenType> {
    if node.is_operator() {
        return Some(M2SemanticTokenType::Operator);
    }

    if node.is::<IntegerLiteral>() || node.is::<FloatLiteral>() {
        Some(M2SemanticTokenType::Number)
    } else if node.is_string_literal() {
        Some(M2SemanticTokenType::String)
    } else if node.is_comment() {
        Some(M2SemanticTokenType::Comment)
    } else if node.is_modifier_token() {
        Some(M2SemanticTokenType::QuoteModifier)
    } else if node.is_keyword_token() {
        Some(M2SemanticTokenType::Keyword)
    } else {
        None
    }
}

fn source_role_semantic_token(
    role: &SourceSemanticRole,
    is_unquoted_symbol: bool,
    source_text: &str,
    knowledge: &(impl SemanticTokenKnowledge + ?Sized),
) -> Option<M2SemanticToken> {
    let token = match role {
        SourceSemanticRole::InstallationHead => return None,
        SourceSemanticRole::RingGenerator => M2SemanticToken::new(M2SemanticTokenType::Variable)
            .with_modifier(M2SemanticTokenModifier::Algebraic),
        SourceSemanticRole::RingGeneratorDeclaration => {
            M2SemanticToken::new(M2SemanticTokenType::Variable)
                .with_modifier(M2SemanticTokenModifier::Algebraic)
                .with_modifier(M2SemanticTokenModifier::Declaration)
        }
        SourceSemanticRole::CodomainAnnotation | SourceSemanticRole::MethodTypeParameter => {
            M2SemanticToken::new(M2SemanticTokenType::TypeAnnotation)
        }
        SourceSemanticRole::OptionKey if is_unquoted_symbol => {
            M2SemanticToken::option_name(source_text, knowledge)
        }
        SourceSemanticRole::OptionKey => M2SemanticToken::new(M2SemanticTokenType::Property)
            .with_modifier(M2SemanticTokenModifier::Option),
        SourceSemanticRole::PropertyKey => M2SemanticToken::new(M2SemanticTokenType::Property),
        SourceSemanticRole::QuotedSymbol => M2SemanticToken::new(M2SemanticTokenType::Symbol),
        SourceSemanticRole::NamespaceArgument => {
            M2SemanticToken::new(M2SemanticTokenType::Namespace)
        }
    };
    Some(token)
}

fn local_symbol_static_type_token(
    symbol: &(impl Metadata + ?Sized),
    type_name: &str,
    knowledge: &(impl SemanticTokenKnowledge + ?Sized),
) -> Option<M2SemanticToken> {
    let token = knowledge.semantic_token_for_static_type(type_name)?;
    match symbol.meta().symbol_kind {
        Some(SymbolKind::VARIABLE)
            if matches!(
                token.token_type,
                M2SemanticTokenType::String
                    | M2SemanticTokenType::Number
                    | M2SemanticTokenType::Symbol
            ) =>
        {
            None
        }
        _ => Some(token),
    }
}

impl ObjectRegistry {
    /// Classify an indexed object by its runtime class and hierarchy for LSP
    /// semantic tokens.
    pub fn get_semantic_token(&self, name: &str) -> Option<M2SemanticToken> {
        semantic_token_from_knowledge(self, name)
    }

    /// Classify a known static type when recoloring a local symbol by inference.
    pub fn get_semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken> {
        semantic_token_for_static_type_from_knowledge(self, type_name)
    }

    /// Whether `name` resolves to a protected object whose class is exactly
    /// `Symbol`.
    pub fn is_protected_symbol(&self, name: &str) -> bool {
        is_protected_symbol(self, name)
    }
}

impl SemanticTokenKnowledge for ObjectRegistry {
    fn semantic_token(&self, name: &str) -> Option<M2SemanticToken> {
        ObjectRegistry::get_semantic_token(self, name)
    }

    fn semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken> {
        ObjectRegistry::get_semantic_token_for_static_type(self, type_name)
    }

    fn is_protected_symbol(&self, name: &str) -> bool {
        ObjectRegistry::is_protected_symbol(self, name)
    }

    fn is_protected_global(&self, name: &str) -> bool {
        is_protected_global(self, name)
    }
}

impl SemanticTokenKnowledge for ObjectRegistryView<'_> {
    fn semantic_token(&self, name: &str) -> Option<M2SemanticToken> {
        semantic_token_from_knowledge(self, name)
    }

    fn semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken> {
        semantic_token_for_static_type_from_knowledge(self, type_name)
    }

    fn is_protected_symbol(&self, name: &str) -> bool {
        is_protected_symbol(self, name)
    }

    fn is_protected_global(&self, name: &str) -> bool {
        is_protected_global(self, name)
    }
}

impl<T: SemanticTokenKnowledge + ?Sized> SemanticTokenKnowledge for &T {
    fn semantic_token(&self, name: &str) -> Option<M2SemanticToken> {
        T::semantic_token(self, name)
    }

    fn semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken> {
        T::semantic_token_for_static_type(self, type_name)
    }

    fn is_protected_symbol(&self, name: &str) -> bool {
        T::is_protected_symbol(self, name)
    }

    fn is_protected_global(&self, name: &str) -> bool {
        T::is_protected_global(self, name)
    }
}

fn is_protected_global(knowledge: &(impl ObjectKnowledge + ?Sized), name: &str) -> bool {
    knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| record.protected)
}

fn is_protected_symbol(knowledge: &(impl ObjectKnowledge + ?Sized), name: &str) -> bool {
    let symbol_type = ObjectName::new("Symbol");
    knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| record.class == symbol_type && record.protected)
}

/// Classify an indexed binding by its runtime class and protection flag.
/// Compiled implementation is independent of name spelling or package.
pub fn semantic_token_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    name: &str,
) -> Option<M2SemanticToken> {
    let record = knowledge.get_record(&ObjectName::new(name))?;
    let mut token = if record_is_type_like(record) {
        M2SemanticToken::new(M2SemanticTokenType::Type)
    } else {
        semantic_token_for_static_type_from_knowledge(knowledge, record.class.as_ref())
            .unwrap_or_else(|| M2SemanticToken::new(M2SemanticTokenType::Variable))
    };
    if record.protected {
        token = token.with_modifier(M2SemanticTokenModifier::Readonly);
    }
    Some(token)
}

/// Classify a value from its inferred static type. Binding protection is
/// deliberately absent: an alias of a protected name can still be assigned.
/// Returns `None` for types that leave the value a plain variable.
pub fn semantic_token_for_static_type_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    type_name: &str,
) -> Option<M2SemanticToken> {
    let type_name = ObjectName::new(type_name);
    let subtype_of = |parent: &str| knowledge.is_subtype(&type_name, &ObjectName::new(parent));

    let token_type = if METHOD_FUNCTION_CLASSES.into_iter().any(subtype_of) {
        M2SemanticTokenType::Method
    } else if subtype_of("Package") {
        M2SemanticTokenType::Namespace
    } else if subtype_of("Type") {
        M2SemanticTokenType::Type
    } else if COMMAND_CLASSES.into_iter().any(subtype_of)
        || subtype_of("Function")
        || subtype_of("ScriptedFunctor")
    {
        M2SemanticTokenType::Function
    } else if subtype_of("File") {
        M2SemanticTokenType::Variable
    } else if subtype_of("Symbol") && !subtype_of("Keyword") && !subtype_of("Operator") {
        M2SemanticTokenType::Symbol
    } else {
        return None;
    };

    let mut token = M2SemanticToken::new(token_type);
    token.compiled_callable = if subtype_of("CompiledFunction") {
        Some(CompiledCallableKind::Function)
    } else if subtype_of("CompiledFunctionClosure") {
        Some(CompiledCallableKind::Closure)
    } else {
        None
    };
    Some(token)
}

fn record_is_type_like(record: &Record) -> bool {
    record.type_info().is_some()
}
