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

use crate::algebraic_kind::AlgebraicKind;
use crate::builtin_index::Record;
use crate::meta::{BindingRole, Metadata};
use crate::object_registry::{ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView};
use crate::settings::SemanticTokenSettings;
use crate::source::DocumentSpan;
use crate::typesystem::TypeKnowledge;
use m2_syn::{LiteralKind, Token, TokenTree};
use serde::Deserialize;
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

/// The function every algebraic object answers: a class is algebraic when
/// `ring` is installed on it, as on `Ideal`, `Module`, `Matrix` or a
/// package's `CoherentSheaf`, so its values live over a ring.
const RING_OF_FUNCTION: &str = "ring";

/// The class of numbers, which `ring` also answers but which read as plain
/// values rather than algebraic objects.
const NUMBER_CLASS: &str = "Number";

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

    /// Whether the visible global object is protected against ordinary assignment.
    fn is_protected_global(&self, name: &str) -> bool;
}

/// The token types, named in Macaulay2 terms and sent as standard LSP
/// types except for custom escapes, delimiters, and punctuation. Each
/// discriminant is its legend index; finer distinctions are
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
    Package = SemanticTokenType::NAMESPACE,
    /// A library object that is a table of the interpreter's own data rather
    /// than a value: a `Dictionary`, such as `OutputDictionary`, a
    /// `HashTable` or `MutableHashTable`, such as `typicalValues`, or a
    /// `NumberedVerticalList`. Sent as `struct`.
    Table = SemanticTokenType::STRUCT,
    /// A name used as a `Symbol` rather than for a value: a protected symbol
    /// (`Strategy`, an option value such as `Linear`) or a quoted name
    /// (`symbol f`). Sent as `enumMember`, the standard type closest to it.
    Symbol = SemanticTokenType::ENUM_MEMBER,
    /// Language keyword classified using the protocol keyword token category.
    Keyword = SemanticTokenType::KEYWORD,
    /// A string-valued selector or source literal.
    String = SemanticTokenType::STRING,
    /// Numeric literal classified using the protocol number token category.
    Number = SemanticTokenType::NUMBER,
    /// An operator callable or its syntax classification.
    Operator = SemanticTokenType::OPERATOR,
    /// A foldable or classified comment span.
    Comment = SemanticTokenType::COMMENT,
    /// A method function: an object made by `method()` that dispatches to
    /// the methods installed on it, such as `det` or `source`.
    Method = SemanticTokenType::METHOD,
    /// The modifier keywords `local`, `global`, `symbol` and their kin.
    QuoteModifier = SemanticTokenType::MODIFIER,
    /// A domain of an installation: the `ZZ` of `f ZZ := List => …`. Sent as
    /// `typeParameter`; the codomain is an [`Annotation`](Self::Annotation).
    TypeAnnotation = SemanticTokenType::TYPE_PARAMETER,
    /// Markup around documentation text: a TeX delimiter, the `{\tt` and
    /// `}` of `{\tt x}`, or a hypertext constructor such as `TT` or `PARA`.
    /// The text it styles stays [`Docs`](Self::Docs) carrying the
    /// style's modifier.
    Macro = SemanticTokenType::MACRO,
    /// An annotation attached to what it decorates: the codomain of an
    /// installation, the `List` of `f ZZ := List => …`, or a link to another
    /// documentation page, such as `TO`, `TOH`, or `TO2`. Sent as `decorator`;
    /// section labels use `keyword`.
    Annotation = SemanticTokenType::DECORATOR,
    /// Regular-expression punctuation classified using the protocol regexp category.
    Regexp = SemanticTokenType::REGEXP,
    /// An escaped character in a pattern, including its source backslashes.
    /// Falls back to `string` under `standardOnly`.
    EscapeSequence = SemanticTokenType::new("escapeSequence"),
    /// Group boundaries in embedded code, such as parentheses and brackets.
    Delimiter = SemanticTokenType::new("delimiter"),
    /// Embedded code separators, such as commas and semicolons.
    Punctuation = SemanticTokenType::new("punctuation"),
    /// Documentation prose, styled independently from source string literals.
    Docs = SemanticTokenType::new("docs"),
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

/// Named semantic modifier available to configuration consumers.
#[derive(Debug, Eq, PartialEq, Hash, PartialOrd, Clone, Deserialize)]
#[allow(
    dead_code,
    reason = "Reserved modifier vocabulary for configuration consumers."
)]
pub struct SemanticModifier(pub SemanticTokenModifier);

#[allow(
    dead_code,
    reason = "Reserved modifier vocabulary for configuration consumers."
)]
impl SemanticModifier {
    /// Marks a binding introduction.
    pub const DECLARATION: SemanticModifier = SemanticModifier(SemanticTokenModifier::DECLARATION);
    /// Marks a definition.
    pub const DEFINITION: SemanticModifier = SemanticModifier(SemanticTokenModifier::DEFINITION);
    /// Marks a protected binding.
    pub const PROTECTED: SemanticModifier = SemanticModifier(SemanticTokenModifier::READONLY);
    /// Marks a deprecated object.
    pub const DEPRECATED: SemanticModifier = SemanticModifier(SemanticTokenModifier::DEPRECATED);
    /// Marks a modified binding.
    pub const MODIFICATION: SemanticModifier =
        SemanticModifier(SemanticTokenModifier::MODIFICATION);
    /// Marks documentation content.
    pub const DOCUMENTATION: SemanticModifier =
        SemanticModifier(SemanticTokenModifier::DOCUMENTATION);
    /// Marks a selected library object.
    pub const DEFAULT_LIBRARY: SemanticModifier =
        SemanticModifier(SemanticTokenModifier::DEFAULT_LIBRARY);
    /// Marks abstract content.
    pub const ABSTRACT: SemanticModifier = SemanticModifier(SemanticTokenModifier::ABSTRACT);
    /// Marks a structural index in an indexed generator name.
    pub const INDEX: SemanticModifier = SemanticModifier(SemanticTokenModifier::new("index"));
    /// Reserved companion to abstract mathematical content.
    pub const NONSENSE: SemanticModifier = SemanticModifier(SemanticTokenModifier::new("nonsense"));
}

/// The modifiers m2-ls attaches to tokens. Like [`M2SemanticTokenType`],
/// the legend lists the variants in declaration order, so a variant's
/// discriminant is its bit position. Some are standard LSP modifiers; the
/// rest are Macaulay2's own; `standardOnly` maps or removes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
#[enum_fn(pub const lsp_modifier -> SemanticTokenModifier)]
#[non_exhaustive]
#[repr(u32)]
pub enum M2SemanticTokenModifier {
    /// An option key, such as `Strategy` in `Strategy => Linear`.
    OptionalParameter = SemanticTokenModifier::new("optionalParameter"),

    /// Where a name is bound — `x := 1`, a ring generator in `QQ[x]` — or
    /// what an installation installs on, the `f` of `f ZZ := …`.
    Declaration = SemanticTokenModifier::DECLARATION,

    /// A value selected by `semanticTokens.defaultLibrary`: by default a
    /// `CompiledFunction`, optionally a compiled closure or package global.
    DefaultLibrary = SemanticTokenModifier::DEFAULT_LIBRARY,

    /// A binding protected from reassignment, independently of value mutability.
    Readonly = SemanticTokenModifier::READONLY,

    /// Standard compatibility fallback for custom documentation style modifiers.
    /// Documentation code does not inherit this modifier automatically.
    Documentation = SemanticTokenModifier::DOCUMENTATION,
    /// An algebraic object that is not itself a type: a generator such as
    /// the `x` of `QQ[x]`, or a value whose class `ring` is installed on,
    /// such as an ideal, a module or a matrix. Rings are types and keep
    /// [`Type`](M2SemanticTokenType::Type).
    Algebraic = SemanticTokenModifier::new("algebraic"),
    /// A ring, which is also a [`Type`](M2SemanticTokenType::Type): `ZZ`,
    /// `R = QQ[x]`, a quotient, a field, a sheaf of rings.
    Ring = SemanticTokenModifier::new("ring"),
    /// A module: an ideal, a free module, a coherent sheaf, a vector bundle.
    Module = SemanticTokenModifier::new("module"),
    /// A morphism: a matrix, a ring map, a map of complexes or sheaves.
    Morphism = SemanticTokenModifier::new("morphism"),
    /// A complex: a chain complex, a resolution, a spectral sequence.
    Complex = SemanticTokenModifier::new("complex"),
    /// A functor such as `Ext`, `Tor` or `HH`, which is a
    /// [`Function`](M2SemanticTokenType::Function) when applied.
    Functor = SemanticTokenModifier::new("functor"),
    /// An algebraic structure other than the above: a monoid, a poset, a
    /// matroid, a Lie algebra.
    Structure = SemanticTokenModifier::new("structure"),
    /// A geometric space: a variety, a polyhedron, a fan, a simplicial complex.
    Space = SemanticTokenModifier::new("space"),
    /// An operator selecting a part of its left operand rather than computing
    /// from both: the `_` of `S_1`, `.` and `.?`, and binary `#` and `#?`.
    MemberAccess = SemanticTokenModifier::new("memberAccess"),
    /// Bold documentation text: `{\bf …}`, `BOLD`, a heading.
    Strong = SemanticTokenModifier::new("strong"),
    /// Italic documentation text: `{\em …}`, `{\it …}`, `EM`, `ITALIC`.
    Emphasis = SemanticTokenModifier::new("emphasis"),
    /// Monospace documentation text: `{\tt …}`, `TT`, `CODE`, `PRE`, a
    /// SimpleDoc `Pre` block.
    Monospace = SemanticTokenModifier::new("monospace"),
    /// The address of a web link: `\url{…}`, `HREF`.
    Url = SemanticTokenModifier::new("url"),
    /// A path to a file: the `"foo.m2"` of `load "foo.m2"`.
    File = SemanticTokenModifier::new("file"),
    /// A path to a directory; reserved, no automatic emission rule.
    Directory = SemanticTokenModifier::new("directory"),
    /// The target of a `[[wikilink]]` in a documentation comment.
    Wikilink = SemanticTokenModifier::new("wikilink"),
    /// TeX mathematics, `$…$`.
    Tex = SemanticTokenModifier::new("tex"),
    /// Code written inside a string and analysed as code: the body of a
    /// `TEST` block, a documentation example.
    InjectedCode = SemanticTokenModifier::new("injectedCode"),
    /// A string naming an object that need not be in scope, such as the
    /// package of `needsPackage "Foo"`.
    InjectedName = SemanticTokenModifier::new("injectedName"),
    /// A documentation tag: the key a `Key` entry documents or a link names,
    /// in all its parts — function, signature, option, operator symbol.
    DocumentTag = SemanticTokenModifier::new("documentTag"),
    /// Code shown or executed as a documentation example.
    Example = SemanticTokenModifier::new("example"),
    /// Code in a `TEST` block.
    Test = SemanticTokenModifier::new("test"),

    /// Reserved standard modifier for abstract content; no automatic emission rule.
    Abstract = SemanticTokenModifier::ABSTRACT,
    /// Structural index in a known indexed generator name; reserved for span emission.
    Index = SemanticTokenModifier::new("index"),
    /// Reserved companion to abstract mathematical content; no automatic emission rule.
    Nonsense = SemanticTokenModifier::new("nonsense"),
    /// An indexed library binding exported as protected, regardless of value mutability.
    LibraryConst = SemanticTokenModifier::new("libraryConst"),
    /// An indexed library binding exported as mutable and available for configuration.
    LibraryConfigParam = SemanticTokenModifier::new("libraryConfigParam"),
}

impl From<AlgebraicKind> for M2SemanticTokenModifier {
    fn from(kind: AlgebraicKind) -> Self {
        match kind {
            AlgebraicKind::Ring => Self::Ring,
            AlgebraicKind::Module => Self::Module,
            AlgebraicKind::Morphism => Self::Morphism,
            AlgebraicKind::Complex => Self::Complex,
            AlgebraicKind::Functor => Self::Functor,
            AlgebraicKind::Structure => Self::Structure,
            AlgebraicKind::Space => Self::Space,
        }
    }
}

impl M2SemanticTokenModifier {
    /// The modifier's bit in an [`M2SemanticTokenModifiers`] set, which is
    /// also its bit on the wire.
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// Standard-only representation, including contextual fallbacks.
    pub const fn standard_fallback(self) -> Option<Self> {
        match self {
            Self::Example
            | Self::DocumentTag
            | Self::Strong
            | Self::Emphasis
            | Self::Monospace
            | Self::Url
            | Self::File
            | Self::Directory
            | Self::Wikilink
            | Self::Tex => Some(Self::Documentation),
            Self::Nonsense
            | Self::Algebraic
            | Self::Ring
            | Self::Module
            | Self::Morphism
            | Self::Complex
            | Self::Functor
            | Self::Structure
            | Self::Space => Some(Self::Abstract),
            modifier if modifier.is_standard() => Some(modifier),
            _ => None,
        }
    }

    /// Whether the modifier is one the LSP specification defines, and so
    /// is still sent under `standardOnly`.
    pub const fn is_standard(self) -> bool {
        matches!(
            self,
            Self::Declaration
                | Self::DefaultLibrary
                | Self::Readonly
                | Self::Documentation
                | Self::Abstract
        )
    }
}

/// Bit set of protocol-standard and Macaulay2-specific token modifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct M2SemanticTokenModifiers(u32);

impl M2SemanticTokenModifiers {
    /// Adds the requested modifier to this immutable bit set.
    pub const fn with(self, modifier: M2SemanticTokenModifier) -> Self {
        Self(self.0 | modifier.bit())
    }

    /// Tests membership in the represented selection or source extent.
    pub const fn contains(self, modifier: M2SemanticTokenModifier) -> bool {
        self.0 & modifier.bit() != 0
    }
}

/// Runtime implementation facts retained across local aliases and workspace
/// indexing so a configuration refresh can choose highlighting without
/// repeating type analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledCallableKind {
    /// An ordinary callable name or its source expression.
    Function,
    /// Function value created from a source lambda.
    Closure,
}

/// A semantic token classification before source coordinates are attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct M2SemanticToken {
    /// Protocol semantic-token category assigned to this span.
    pub token_type: M2SemanticTokenType,
    /// Independent semantic properties attached to the token.
    pub modifiers: M2SemanticTokenModifiers,
    /// Compiled callable form recorded by the runtime catalog.
    pub compiled_callable: Option<CompiledCallableKind>,
}

impl M2SemanticToken {
    /// Creates a token classification with the supplied category and no modifiers.
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

    /// The token of an option name written as a symbol: a
    /// [`Property`](M2SemanticTokenType::Property) of the options it is a key
    /// of, protected or not, so every key of a `k => v` pair reads alike.
    pub fn option_name(name: &str, knowledge: &(impl SemanticTokenKnowledge + ?Sized)) -> Self {
        Self::new(M2SemanticTokenType::Property)
            .with_modifier(M2SemanticTokenModifier::OptionalParameter)
            .with_binding_protection(knowledge.is_protected_global(name))
    }

    /// Adds one semantic modifier without changing the token category.
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
    /// modifier bitset, mapping M2-specific classifications under
    /// `standardOnly`; custom types use their standard fallbacks.
    pub fn encode(self, settings: &SemanticTokenSettings) -> (u32, u32) {
        if !settings.standard_only {
            return (self.token_type as u32, self.modifiers.0);
        }
        let modifiers = M2SemanticTokenModifier::iter()
            .filter(|modifier| self.modifiers.contains(*modifier))
            .filter_map(M2SemanticTokenModifier::standard_fallback)
            .fold(
                M2SemanticTokenModifiers::default(),
                M2SemanticTokenModifiers::with,
            );
        let token_type = match self.token_type {
            M2SemanticTokenType::EscapeSequence | M2SemanticTokenType::Docs => {
                M2SemanticTokenType::String
            }
            M2SemanticTokenType::Delimiter | M2SemanticTokenType::Punctuation => {
                M2SemanticTokenType::Operator
            }
            token_type => token_type,
        };
        (token_type as u32, modifiers.0)
    }
}

/// The syntactic role of one source occurrence, recorded by analysis when
/// its position changes how it should be highlighted. Unlike an inferred
/// runtime class, this describes the use of a name: `f` in `symbol f` is a
/// quoted symbol even if its binding holds a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSemanticRole {
    /// A variable introduced by a ring constructor.
    RingGenerator,
    /// A literal promoted into a ring by subscript, `3_R` or `1.5_RR`: one
    /// number, its ring part of how it is written.
    RingLiteral,
    /// An inexact field of a written precision, `RR_53` or `CC_100`: one
    /// ring, like the `RR` it is drawn from.
    PrecisionField,
    /// A ring generator where a constructor declares it: the `x`, `y_1` and
    /// `z_0 .. z_2` of `QQ[x, y_1, z_0 .. z_2]`, quoted or not.
    RingGeneratorDeclaration,
    /// A domain or codomain type in a method installation.
    MethodTypeParameter,
    /// What an installation installs on, the `f` of `f ZZ := …` or the `+`
    /// of `T + T := …`: highlighted as it is elsewhere, marked as defining.
    InstallationHead,
    /// The codomain an installation annotates, the `List` of
    /// `f ZZ := List => x -> …`: Macaulay2's syntax for stating the
    /// installation's typical value.
    CodomainAnnotation,
    /// A name denoting a symbol that serves as an option name.
    OptionKey,
    /// What an access selects by name or position: the `key` of `H.key`,
    /// the `0` of `L_0` or `L#0`.
    PropertyKey,
    /// The operator of an access, the `_` of `L_0` or the `.` of `H.key`:
    /// it joins what is selected to what it is selected from.
    AccessOperator,
    /// A string used as a regular-expression pattern.
    RegularExpression,
    /// A string naming a loaded package.
    NamespaceArgument,
    /// The path a `load`/`needs`/`input` call runs, the `"foo.m2"` of
    /// `load "foo.m2"`: a source file brought in the way a package is,
    /// highlighted as a namespace but not navigated as a package name.
    LoadedFilePath,
    /// The name a quote yields as a `Symbol` rather than evaluating —
    /// `symbol f`, `global f`, `local f`, `threadVariable f`, `threadLocal f`.
    /// The quote wins over whatever `f` is otherwise bound to.
    QuotedSymbol,
}

/// One token of the source with what the syntax around it says about its
/// classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSemanticToken {
    /// Source coordinates associated with this item.
    pub span: DocumentSpan,
    /// Classification derived directly from syntax, when one applies.
    pub syntax_token_type: Option<M2SemanticTokenType>,
    /// Semantic role assigned by source analysis, when one applies.
    pub source_role: Option<SourceSemanticRole>,
    /// Whether the source span denotes a symbol.
    pub is_symbol: bool,
    /// Whether the symbol is evaluated rather than returned by quotation.
    pub is_unquoted_symbol: bool,
    /// Whether the symbol occurs in an expression value position.
    pub is_expression_symbol: bool,
    /// Whether this expression supplies a control-flow condition.
    pub is_condition_value: bool,
}

/// Positioned syntax and binding evidence used to classify a source token.
pub struct SourceSemanticTokenContext<'a, M: ?Sized> {
    /// Original document text used for token coordinates and classification.
    pub source_text: &'a str,
    /// Source classification being refined with binding and workspace facts.
    pub source_token: &'a SourceSemanticToken,
    /// The scoped binding to which these facts apply.
    pub binding: Option<&'a M>,
    /// Whether this occurrence introduces the selected binding.
    pub is_declaration: bool,
    /// Cross-file classification, when available.
    pub workspace_token: Option<M2SemanticToken>,
    /// Whether syntax-only classifications should be emitted for this client.
    pub emit_syntax: bool,
}

/// Combines syntax, binding, and workspace evidence into a token classification.
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
        .and_then(|role| source_role_semantic_token(role, context.source_text, knowledge));

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

/// Classifies a locally bound symbol using its semantic metadata.
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
        Some(SymbolKind::NAMESPACE) => M2SemanticTokenType::Package,
        Some(SymbolKind::PROPERTY) => M2SemanticTokenType::Property,
        Some(SymbolKind::CONSTANT) => M2SemanticTokenType::Symbol,
        _ => M2SemanticTokenType::Variable,
    })
}

/// Separators, which are punctuation but no operator.
const SEPARATORS: [&str; 2] = [<Token![,]>::SPELLING, <Token![;]>::SPELLING];

/// The classes whose library instances are [`Table`](M2SemanticTokenType::Table)s.
/// `MutableHashTable` is a `HashTable`.
const TABLE_CLASSES: [&str; 3] = ["Dictionary", "HashTable", "NumberedVerticalList"];

/// Keywords selecting how a quote binds its symbol.
const QUOTE_MODIFIERS: [&str; 5] = [
    <Token![global]>::SPELLING,
    <Token![local]>::SPELLING,
    <Token![symbol]>::SPELLING,
    <Token![threadLocal]>::SPELLING,
    <Token![threadVariable]>::SPELLING,
];

/// Operators spelled as words.
const WORD_OPERATORS: [&str; 4] = [
    <Token![and]>::SPELLING,
    <Token![or]>::SPELLING,
    <Token![not]>::SPELLING,
    <Token![xor]>::SPELLING,
];

/// Token category determined solely by the token, absent for delimited groups
/// and separators. A word is a keyword here; whether it is a symbol instead
/// is for the syntax around it to decide.
pub fn lexical_semantic_token_type(token: &TokenTree) -> Option<M2SemanticTokenType> {
    Some(match token {
        TokenTree::Group(_) => return None,
        TokenTree::Literal(literal) => match literal.kind {
            LiteralKind::Integer | LiteralKind::Float => M2SemanticTokenType::Number,
            LiteralKind::String | LiteralKind::RawString => M2SemanticTokenType::String,
        },
        TokenTree::Punct(punct) if SEPARATORS.contains(&punct.text()) => return None,
        TokenTree::Punct(_) => M2SemanticTokenType::Operator,
        TokenTree::Ident(ident) => {
            let word = ident.text();
            if QUOTE_MODIFIERS.contains(&word) {
                M2SemanticTokenType::QuoteModifier
            } else if WORD_OPERATORS.contains(&word) {
                M2SemanticTokenType::Operator
            } else {
                M2SemanticTokenType::Keyword
            }
        }
    })
}

/// Token category and modifiers implied by a recorded semantic source role.
fn source_role_semantic_token(
    role: &SourceSemanticRole,
    source_text: &str,
    knowledge: &(impl SemanticTokenKnowledge + ?Sized),
) -> Option<M2SemanticToken> {
    let token = match role {
        SourceSemanticRole::InstallationHead => return None,
        SourceSemanticRole::RegularExpression => M2SemanticToken::new(M2SemanticTokenType::Regexp),
        SourceSemanticRole::RingGenerator => M2SemanticToken::new(M2SemanticTokenType::Variable)
            .with_modifier(M2SemanticTokenModifier::Algebraic),
        SourceSemanticRole::RingGeneratorDeclaration => {
            M2SemanticToken::new(M2SemanticTokenType::Variable)
                .with_modifier(M2SemanticTokenModifier::Algebraic)
                .with_modifier(M2SemanticTokenModifier::Declaration)
        }
        SourceSemanticRole::RingLiteral => M2SemanticToken::new(M2SemanticTokenType::Number),
        SourceSemanticRole::PrecisionField => M2SemanticToken::new(M2SemanticTokenType::Type),
        SourceSemanticRole::CodomainAnnotation => {
            M2SemanticToken::new(M2SemanticTokenType::Annotation)
        }
        SourceSemanticRole::MethodTypeParameter => {
            M2SemanticToken::new(M2SemanticTokenType::TypeAnnotation)
        }
        SourceSemanticRole::OptionKey => M2SemanticToken::option_name(source_text, knowledge),
        SourceSemanticRole::PropertyKey => M2SemanticToken::new(M2SemanticTokenType::Property),
        SourceSemanticRole::AccessOperator => M2SemanticToken::new(M2SemanticTokenType::Operator)
            .with_modifier(M2SemanticTokenModifier::MemberAccess),
        SourceSemanticRole::QuotedSymbol => M2SemanticToken::new(M2SemanticTokenType::Symbol),
        SourceSemanticRole::NamespaceArgument => M2SemanticToken::new(M2SemanticTokenType::Package)
            .with_modifier(M2SemanticTokenModifier::InjectedName),
        SourceSemanticRole::LoadedFilePath => M2SemanticToken::new(M2SemanticTokenType::Package)
            .with_modifier(M2SemanticTokenModifier::File),
    };
    Some(token)
}

/// Semantic classification implied by the local binding's inferred runtime class.
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
}

impl SemanticTokenKnowledge for ObjectRegistry {
    fn semantic_token(&self, name: &str) -> Option<M2SemanticToken> {
        ObjectRegistry::get_semantic_token(self, name)
    }

    fn semantic_token_for_static_type(&self, type_name: &str) -> Option<M2SemanticToken> {
        ObjectRegistry::get_semantic_token_for_static_type(self, type_name)
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

    fn is_protected_global(&self, name: &str) -> bool {
        T::is_protected_global(self, name)
    }
}

/// Whether the visible global object is protected against ordinary assignment.
fn is_protected_global(knowledge: &(impl ObjectKnowledge + ?Sized), name: &str) -> bool {
    knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| record.protected)
}

/// Classify an indexed binding by its runtime class and protection flag.
/// Compiled implementation is independent of name spelling or package.
pub fn semantic_token_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    name: &str,
) -> Option<M2SemanticToken> {
    let record = knowledge.get_record(&ObjectName::new(name))?;
    let mut token = if record_is_type_like(record) {
        let token = M2SemanticToken::new(M2SemanticTokenType::Type);
        match AlgebraicKind::of(knowledge, &ObjectName::new(record.class.as_ref())) {
            Some(kind) => token.with_modifier(kind.into()),
            None => token,
        }
    } else {
        semantic_token_for_static_type_from_knowledge(knowledge, record.class.as_ref())
            .or_else(|| {
                is_table_class(knowledge, record.class.as_ref())
                    .then(|| M2SemanticToken::new(M2SemanticTokenType::Table))
            })
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
    let kind = AlgebraicKind::of(knowledge, &type_name).map(M2SemanticTokenModifier::from);

    let token_type = if METHOD_FUNCTION_CLASSES.into_iter().any(subtype_of) {
        M2SemanticTokenType::Method
    } else if subtype_of("Package") {
        M2SemanticTokenType::Package
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
    } else if let Some(kind) = kind {
        return Some(M2SemanticToken::new(M2SemanticTokenType::Variable).with_modifier(kind));
    } else if !subtype_of(NUMBER_CLASS) && lives_over_a_ring(knowledge, &type_name) {
        return Some(
            M2SemanticToken::new(M2SemanticTokenType::Variable)
                .with_modifier(M2SemanticTokenModifier::Algebraic),
        );
    } else {
        return None;
    };

    let mut token = M2SemanticToken::new(token_type);
    if let Some(kind) = kind {
        token = token.with_modifier(kind);
    }
    token.compiled_callable = if subtype_of("CompiledFunction") {
        Some(CompiledCallableKind::Function)
    } else if subtype_of("CompiledFunctionClosure") {
        Some(CompiledCallableKind::Closure)
    } else {
        None
    };
    Some(token)
}

/// Whether `class` is one of the interpreter's table classes, whose library
/// instances are tables of its own data.
fn is_table_class(knowledge: &(impl TypeKnowledge + ?Sized), class: &str) -> bool {
    let class = ObjectName::new(class);
    TABLE_CLASSES
        .into_iter()
        .any(|table| knowledge.is_subtype(&class, &ObjectName::new(table)))
}

/// Whether `ring` is installed on `class` or one of its ancestors.
fn lives_over_a_ring(knowledge: &(impl TypeKnowledge + ?Sized), class: &ObjectName) -> bool {
    let Some(class) = knowledge.resolve_type_id(class) else {
        return false;
    };
    knowledge
        .get_record(&ObjectName::new(RING_OF_FUNCTION))
        .and_then(Record::callable)
        .is_some_and(|ring| {
            ring.methods
                .iter()
                .any(|method| match method.domain.as_slice() {
                    [domain] => knowledge
                        .type_id(domain)
                        .is_some_and(|domain| knowledge.is_subtype_id(&class, &domain)),
                    _ => false,
                })
        })
}

/// Whether a catalog record denotes a type rather than an ordinary value.
fn record_is_type_like(record: &Record) -> bool {
    record.type_info().is_some()
}
