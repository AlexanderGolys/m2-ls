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
//! | `CompiledFunction` of `Core` | `apply`, `scan` | `function` + `defaultLibrary` |
//! | other `CompiledFunction`, `CompiledFunctionClosure` | | `function` |
//! | `ScriptedFunctor` | `id`, `Ext`, `HH` | `function` |
//! | `Command`, `Manipulator` | `exit`, `help`; `close`, `endl` | `function` + `command` |
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
//! domain `Matrix` is a [`DomainType`](M2SemanticTokenType::DomainType).

use crate::builtin_index::OptionFacts;
use crate::builtin_index::Record;
use crate::meta::{BindingRole, Metadata};
use crate::node_metadata::SyntaxNode;
use crate::object_registry::{ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView};
use crate::settings::SemanticTokenSettings;
use crate::source::DocumentSpan;
use crate::typesystem::TypeKnowledge;
use m2_syn::nodes::{FloatLiteral, IntegerLiteral};
use strum::{EnumIter, IntoEnumIterator};
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

    /// Whether indexed facts admit `value_name` for `option_key`.
    fn is_option_value_for_key(&self, option_key: &str, value_name: &str) -> bool;

    /// Whether `name` is an indexed global the sessions may assign: one its
    /// package exports with `exportMutable`, which leaves it unprotected.
    fn is_mutable_global(&self, name: &str) -> bool;
}

/// The token types, named in Macaulay2 terms and each sent as a standard
/// LSP type. Each discriminant is its legend index; finer distinctions are
/// carried by [modifiers](M2SemanticTokenModifier). The module documentation
/// says which objects are functions and which are methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
#[repr(u32)]
pub enum M2SemanticTokenType {
    /// A type: any object whose class is a `Type`, such as `ZZ`, `Matrix`
    /// or a type a package defines.
    Type,
    /// An object applied to arguments that is not a method function: a
    /// function closure, a compiled function, a scripted functor, or a
    /// command or manipulator (with the
    /// [`Command`](M2SemanticTokenModifier::Command) modifier).
    Function,
    /// Any other value: a local, a global, or a library object such as
    /// `stdio`.
    Variable,
    /// A parameter of a function, or an input a documentation page names.
    Parameter,
    /// A key of an object rather than a binding of its own, such as an
    /// option name that is not a protected symbol.
    Property,
    /// A package.
    Namespace,
    /// A name used as a `Symbol` rather than for a value: a protected symbol
    /// (`Strategy`, an option value such as `Linear`) or a quoted name
    /// (`symbol f`). Sent as `enumMember`, the standard type closest to it.
    Symbol,
    Keyword,
    String,
    Number,
    Operator,
    Comment,
    /// A method function: an object made by `method()` that dispatches to
    /// the methods installed on it, such as `det` or `source`.
    Method,
    /// The modifier keywords `local`, `global`, `symbol` and their kin.
    Modifier,
    /// A domain an installation is made on, the `ZZ` of `f ZZ := …`. Sent
    /// as `typeParameter`.
    DomainType,
    /// Markup around documentation text: a TeX delimiter, the `{\tt` and
    /// `}` of `{\tt x}`, or a hypertext constructor such as `TT` or `PARA`.
    /// The text it styles stays a [`String`](Self::String) carrying the
    /// style's modifier.
    Macro,
    /// An annotation: the codomain an installation states, a SimpleDoc
    /// section keyword such as `Key` or `Description`, or a link to another
    /// documentation page, `TO`, `TOH` or `TO2`. Sent as `decorator`.
    Annotation,
}

impl M2SemanticTokenType {
    pub const fn emit_with_syntax_highlighting(self) -> bool {
        matches!(self, Self::Modifier)
    }

    /// The type's entry in the full legend.
    pub const fn lsp_type(self) -> SemanticTokenType {
        match self {
            Self::Type => SemanticTokenType::TYPE,
            Self::Function => SemanticTokenType::FUNCTION,
            Self::Variable => SemanticTokenType::VARIABLE,
            Self::Parameter => SemanticTokenType::PARAMETER,
            Self::Property => SemanticTokenType::PROPERTY,
            Self::Namespace => SemanticTokenType::NAMESPACE,
            Self::Symbol => SemanticTokenType::ENUM_MEMBER,
            Self::Keyword => SemanticTokenType::KEYWORD,
            Self::String => SemanticTokenType::STRING,
            Self::Number => SemanticTokenType::NUMBER,
            Self::Operator => SemanticTokenType::OPERATOR,
            Self::Comment => SemanticTokenType::COMMENT,
            Self::Method => SemanticTokenType::METHOD,
            Self::Modifier => SemanticTokenType::MODIFIER,
            Self::DomainType => SemanticTokenType::TYPE_PARAMETER,
            Self::Macro => SemanticTokenType::MACRO,
            Self::Annotation => SemanticTokenType::DECORATOR,
        }
    }
}

/// The modifiers m2-ls attaches to tokens. Like [`M2SemanticTokenType`],
/// the legend lists the variants in declaration order, so a variant's
/// discriminant is its bit position. Some are standard LSP modifiers; the
/// rest are Macaulay2's own, and `standardOnly` drops them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
#[repr(u32)]
pub enum M2SemanticTokenModifier {
    /// An option name or an option value, as in `Strategy => Linear`.
    Option,
    /// A `File` object, such as `stdio`.
    File,
    /// Where a name is bound — `x := 1`, a ring generator in `QQ[x]` — or
    /// what an installation installs on, the `f` of `f ZZ := …`.
    Declaration,
    /// A name with nothing bound to it in scope, which evaluates to its own
    /// `Symbol`.
    Unresolved,
    /// A function compiled into the Macaulay2 interpreter and exported by
    /// `Core`, such as `apply`.
    DefaultLibrary,
    /// A symbol a package exports with `exportMutable`, which the sessions
    /// loading it may assign.
    Mutable,
    /// Documentation: a documentation call with its tags and values, the
    /// string a `doc` call reads as SimpleDoc, an attached documentation
    /// comment, and the code they embed.
    Documentation,
    /// A `Command` or `Manipulator`, presented as a function.
    Command,
    /// A generator introduced by an algebraic constructor, such as the `x`
    /// of `QQ[x]`.
    Algebraic,
    /// Bold documentation text: `{\bf …}`, `BOLD`, a heading.
    Strong,
    /// Italic documentation text: `{\em …}`, `{\it …}`, `EM`, `ITALIC`.
    Emphasis,
    /// Monospace documentation text: `{\tt …}`, `TT`, `CODE`, `PRE`, a
    /// SimpleDoc `Pre` block.
    Monospace,
    /// The address of a link: `\url{…}`, `HREF`.
    Link,
    /// TeX mathematics, `$…$`.
    Math,
    /// Code shown or executed as a documentation example.
    Example,
    /// Code in a `TEST` block.
    Test,
}

impl M2SemanticTokenModifier {
    /// The modifier's bit in an [`M2SemanticTokenModifiers`] set, which is
    /// also its bit on the wire.
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// The modifier's entry in the full legend.
    pub const fn lsp_modifier(self) -> SemanticTokenModifier {
        match self {
            Self::Option => SemanticTokenModifier::new("option"),
            Self::File => SemanticTokenModifier::new("file"),
            Self::Declaration => SemanticTokenModifier::DECLARATION,
            Self::Unresolved => SemanticTokenModifier::new("unresolved"),
            Self::DefaultLibrary => SemanticTokenModifier::DEFAULT_LIBRARY,
            Self::Mutable => SemanticTokenModifier::new("mutable"),
            Self::Documentation => SemanticTokenModifier::DOCUMENTATION,
            Self::Command => SemanticTokenModifier::new("command"),
            Self::Algebraic => SemanticTokenModifier::new("algebraic"),
            Self::Strong => SemanticTokenModifier::new("strong"),
            Self::Emphasis => SemanticTokenModifier::new("emphasis"),
            Self::Monospace => SemanticTokenModifier::new("monospace"),
            Self::Link => SemanticTokenModifier::new("link"),
            Self::Math => SemanticTokenModifier::new("math"),
            Self::Example => SemanticTokenModifier::new("example"),
            Self::Test => SemanticTokenModifier::new("test"),
        }
    }

    /// Whether the modifier is one the LSP specification defines, and so
    /// is still sent under `standardOnly`.
    pub const fn is_standard(self) -> bool {
        match self {
            Self::Declaration | Self::DefaultLibrary | Self::Documentation => true,
            Self::Option
            | Self::File
            | Self::Unresolved
            | Self::Mutable
            | Self::Command
            | Self::Algebraic
            | Self::Strong
            | Self::Emphasis
            | Self::Monospace
            | Self::Link
            | Self::Math
            | Self::Example
            | Self::Test => false,
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct M2SemanticToken {
    pub token_type: M2SemanticTokenType,
    pub modifiers: M2SemanticTokenModifiers,
}

impl M2SemanticToken {
    pub const fn new(token_type: M2SemanticTokenType) -> Self {
        Self {
            token_type,
            modifiers: M2SemanticTokenModifiers(0),
        }
    }

    /// A name with no binding, indexed object, or workspace definition in
    /// scope; at runtime it evaluates to its own `Symbol`.
    pub const fn unresolved() -> Self {
        Self::new(M2SemanticTokenType::Variable).with_modifier(M2SemanticTokenModifier::Unresolved)
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
        Self::new(token_type).with_modifier(M2SemanticTokenModifier::Option)
    }

    pub const fn with_modifier(mut self, modifier: M2SemanticTokenModifier) -> Self {
        self.modifiers = self.modifiers.with(modifier);
        self
    }

    /// The legend the `initialize` response states: every type and modifier.
    pub fn legend() -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: M2SemanticTokenType::iter()
                .map(M2SemanticTokenType::lsp_type)
                .collect(),
            token_modifiers: M2SemanticTokenModifier::iter()
                .map(M2SemanticTokenModifier::lsp_modifier)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSemanticRole {
    RingGenerator,
    MethodTypeParameter,
    /// What an installation installs on, the `f` of `f ZZ := …` or the `+`
    /// of `T + T := …`: highlighted as it is elsewhere, marked as defining.
    InstallationHead,
    /// The codomain an installation annotates, the `List` of
    /// `f ZZ := List => x -> …`: Macaulay2's syntax for stating the
    /// installation's typical value.
    CodomainAnnotation,
    OptionKey,
    OptionValue(ObjectName),
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
    pub workspace_token_type: Option<M2SemanticTokenType>,
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
    token = token.or_else(|| context.workspace_token_type.map(M2SemanticToken::new));
    if token.is_none()
        && context.source_token.is_expression_symbol
        && context.source_token.syntax_token_type.is_none()
    {
        token = Some(M2SemanticToken::unresolved());
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
        Some(M2SemanticTokenType::Modifier)
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
        SourceSemanticRole::CodomainAnnotation => {
            M2SemanticToken::new(M2SemanticTokenType::Annotation)
        }
        SourceSemanticRole::MethodTypeParameter => {
            M2SemanticToken::new(M2SemanticTokenType::DomainType)
        }
        SourceSemanticRole::OptionKey if is_unquoted_symbol => {
            M2SemanticToken::option_name(source_text, knowledge)
        }
        SourceSemanticRole::OptionKey => M2SemanticToken::new(M2SemanticTokenType::Property)
            .with_modifier(M2SemanticTokenModifier::Option),
        SourceSemanticRole::OptionValue(option_key) => {
            if !knowledge.is_option_value_for_key(option_key.name(), source_text) {
                return None;
            }
            M2SemanticToken::new(M2SemanticTokenType::Symbol)
                .with_modifier(M2SemanticTokenModifier::Option)
        }
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

    /// Whether the indexed facts admit `value_name` for any spelling of
    /// `option_key`, ignoring package qualification.
    pub fn is_option_value_for_key(&self, option_key: &str, value_name: &str) -> bool {
        is_option_value_for_key(self, self.option_facts(), option_key, value_name)
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

    fn is_option_value_for_key(&self, option_key: &str, value_name: &str) -> bool {
        ObjectRegistry::is_option_value_for_key(self, option_key, value_name)
    }

    fn is_mutable_global(&self, name: &str) -> bool {
        is_mutable_global(self, name)
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

    fn is_option_value_for_key(&self, option_key: &str, value_name: &str) -> bool {
        is_option_value_for_key(self, self.option_facts(), option_key, value_name)
    }

    fn is_mutable_global(&self, name: &str) -> bool {
        is_mutable_global(self, name)
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

    fn is_option_value_for_key(&self, option_key: &str, value_name: &str) -> bool {
        T::is_option_value_for_key(self, option_key, value_name)
    }

    fn is_mutable_global(&self, name: &str) -> bool {
        T::is_mutable_global(self, name)
    }
}

fn is_mutable_global(knowledge: &(impl ObjectKnowledge + ?Sized), name: &str) -> bool {
    knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| !record.protected)
}

fn is_protected_symbol(knowledge: &(impl ObjectKnowledge + ?Sized), name: &str) -> bool {
    let symbol_type = ObjectName::new("Symbol");
    knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| record.class == symbol_type && record.protected)
}

fn is_option_value_for_key(
    knowledge: &(impl ObjectKnowledge + ?Sized),
    option_facts: &OptionFacts,
    option_key: &str,
    value_name: &str,
) -> bool {
    let option = knowledge
        .get_record(&ObjectName::new(option_key))
        .map_or_else(|| ObjectName::new(option_key), |record| record.name.clone());
    let value = knowledge
        .get_record(&ObjectName::new(value_name))
        .map_or_else(|| ObjectName::new(value_name), |record| record.name.clone());

    option_facts
        .option_values_by_slot
        .iter()
        .filter(|(slot, _)| slot.option == option && knowledge.get_record(&slot.callable).is_some())
        .any(|(_, values)| values.contains(&value))
}

/// Classify one indexed object by the runtime class its record states, as
/// the module documentation tabulates. A `Core` compiled function is
/// classified only under its primary name, so an alias stays unmarked.
pub fn semantic_token_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    name: &str,
) -> Option<M2SemanticToken> {
    let record = knowledge.get_record(&ObjectName::new(name))?;
    let data_type = &record.class;
    let subtype_of = |parent: &str| knowledge.is_subtype(data_type, &ObjectName::new(parent));

    let is_file = subtype_of("File");
    let is_primary_core_compiled_function = data_type.as_ref() == "CompiledFunction"
        && record.name.name() == name
        && knowledge
            .object(&record.package)
            .is_some_and(|package| package.name.name() == "Core");

    if is_primary_core_compiled_function {
        return Some(
            M2SemanticToken::new(M2SemanticTokenType::Function)
                .with_modifier(M2SemanticTokenModifier::DefaultLibrary),
        );
    }

    if record_is_type_like(record) {
        return Some(indexed_semantic_token(M2SemanticTokenType::Type, false));
    }

    if COMMAND_CLASSES.into_iter().any(subtype_of) {
        Some(
            M2SemanticToken::new(M2SemanticTokenType::Function)
                .with_modifier(M2SemanticTokenModifier::Command),
        )
    } else if METHOD_FUNCTION_CLASSES.into_iter().any(subtype_of) {
        Some(indexed_semantic_token(M2SemanticTokenType::Method, false))
    } else if subtype_of("Function") || subtype_of("ScriptedFunctor") {
        Some(indexed_semantic_token(M2SemanticTokenType::Function, false))
    } else if subtype_of("Package") {
        Some(indexed_semantic_token(
            M2SemanticTokenType::Namespace,
            false,
        ))
    } else if (subtype_of("Symbol") || is_file) && !subtype_of("Keyword") && !subtype_of("Operator")
    {
        let is_symbol_class = data_type.as_ref() == "Symbol";
        let token_type = if is_symbol_class {
            M2SemanticTokenType::Symbol
        } else {
            M2SemanticTokenType::Variable
        };

        Some(indexed_semantic_token(token_type, is_file))
    } else {
        Some(indexed_semantic_token(M2SemanticTokenType::Variable, false))
    }
}

/// Classify a source object from its inferred static type, by the same
/// classes as [`semantic_token_from_knowledge`]; `None` for a type that
/// leaves the object a plain variable.
pub fn semantic_token_for_static_type_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    type_name: &str,
) -> Option<M2SemanticToken> {
    let type_name = ObjectName::new(type_name);
    let subtype_of = |parent: &str| knowledge.is_subtype(&type_name, &ObjectName::new(parent));
    let is_file = subtype_of("File");
    let is_command = COMMAND_CLASSES.into_iter().any(subtype_of);

    let token_type = if METHOD_FUNCTION_CLASSES.into_iter().any(subtype_of) {
        M2SemanticTokenType::Method
    } else if subtype_of("Package") {
        M2SemanticTokenType::Namespace
    } else if subtype_of("Type") {
        M2SemanticTokenType::Type
    } else if is_command || subtype_of("Function") || subtype_of("ScriptedFunctor") {
        M2SemanticTokenType::Function
    } else if is_file {
        M2SemanticTokenType::Variable
    } else if subtype_of("Symbol") {
        M2SemanticTokenType::Symbol
    } else {
        return None;
    };

    let token = indexed_semantic_token(token_type, is_file);
    Some(if is_command {
        token.with_modifier(M2SemanticTokenModifier::Command)
    } else {
        token
    })
}

fn indexed_semantic_token(token_type: M2SemanticTokenType, is_file: bool) -> M2SemanticToken {
    let token = M2SemanticToken::new(token_type);
    if is_file {
        token.with_modifier(M2SemanticTokenModifier::File)
    } else {
        token
    }
}

fn record_is_type_like(record: &Record) -> bool {
    record.type_info().is_some()
}
