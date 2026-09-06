//! Semantic-token classification over inferred and indexed object facts.

use crate::builtin_index::OptionFacts;
use crate::builtin_index::Record;
use crate::meta::{BindingRole, Metadata};
use crate::node_metadata::M2Node;
use crate::object_registry::{ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView};
use crate::source::DocumentSpan;
use crate::typesystem::{TypeKnowledge, TypeRole};
use m2_syn::nodes::{FloatLiteral, IntegerLiteral};
use tower_lsp::lsp_types::{SemanticTokenModifier, SemanticTokenType, SymbolKind};

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
}

/// The LSP-standard token types emitted for M2 syntax and indexed metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum M2SemanticTokenType {
    Type = 0,
    Function = 1,
    Variable = 2,
    Parameter = 3,
    Property = 4,
    Namespace = 5,
    EnumMember = 6,
    Class = 7,
    Keyword = 8,
    String = 9,
    Number = 10,
    Operator = 11,
    Comment = 12,
    Method = 13,
    Modifier = 14,
    TypeParameter = 15,
}

impl M2SemanticTokenType {
    pub const fn emit_with_syntax_highlighting(self) -> bool {
        matches!(self, Self::Modifier)
    }
}

pub const LEGEND_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::TYPE,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::CLASS,
    SemanticTokenType::KEYWORD,
    SemanticTokenType::STRING,
    SemanticTokenType::NUMBER,
    SemanticTokenType::OPERATOR,
    SemanticTokenType::COMMENT,
    SemanticTokenType::METHOD,
    SemanticTokenType::MODIFIER,
    SemanticTokenType::TYPE_PARAMETER,
    SemanticTokenType::DECORATOR,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum M2SemanticTokenModifier {
    Option,
    Command,
    File,
    Declaration,
    Builtin,
}

impl M2SemanticTokenModifier {
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

pub const LEGEND_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::new("option"),
    SemanticTokenModifier::new("command"),
    SemanticTokenModifier::new("file"),
    SemanticTokenModifier::DECLARATION,
    SemanticTokenModifier::new("builtin"),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct M2SemanticTokenModifiers(u32);

impl M2SemanticTokenModifiers {
    pub const fn with(self, modifier: M2SemanticTokenModifier) -> Self {
        Self(self.0 | modifier.bit())
    }

    pub const fn bits(self) -> u32 {
        self.0
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

    pub const fn with_modifier(mut self, modifier: M2SemanticTokenModifier) -> Self {
        self.modifiers = self.modifiers.with(modifier);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSemanticRole {
    MethodTypeParameter,
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

    let mut token = context.source_token.source_role.as_ref().and_then(|role| {
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

    token = token.or(indexed_token);
    token = token.or_else(|| context.workspace_token_type.map(M2SemanticToken::new));
    if token.is_none()
        && context.source_token.is_expression_symbol
        && context.source_token.syntax_token_type.is_none()
    {
        token = Some(M2SemanticToken::new(M2SemanticTokenType::EnumMember));
    }
    if token.is_none() && context.emit_syntax {
        token = context
            .source_token
            .syntax_token_type
            .map(M2SemanticToken::new);
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
        Some(SymbolKind::CLASS) => M2SemanticTokenType::Class,
        Some(SymbolKind::NAMESPACE) => M2SemanticTokenType::Namespace,
        Some(SymbolKind::PROPERTY) => M2SemanticTokenType::Property,
        Some(SymbolKind::CONSTANT) => M2SemanticTokenType::EnumMember,
        _ => M2SemanticTokenType::Variable,
    })
}

pub fn syntax_semantic_token_type(node: M2Node<'_>) -> Option<M2SemanticTokenType> {
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
        SourceSemanticRole::MethodTypeParameter => {
            M2SemanticToken::new(M2SemanticTokenType::TypeParameter)
        }
        SourceSemanticRole::OptionKey => {
            let token_type = if is_unquoted_symbol && knowledge.is_protected_symbol(source_text) {
                M2SemanticTokenType::EnumMember
            } else {
                M2SemanticTokenType::Property
            };
            M2SemanticToken::new(token_type).with_modifier(M2SemanticTokenModifier::Option)
        }
        SourceSemanticRole::OptionValue(option_key) => {
            if !knowledge.is_option_value_for_key(option_key.name(), source_text) {
                return None;
            }
            M2SemanticToken::new(M2SemanticTokenType::EnumMember)
                .with_modifier(M2SemanticTokenModifier::Option)
        }
        SourceSemanticRole::PropertyKey => M2SemanticToken::new(M2SemanticTokenType::Property),
        SourceSemanticRole::QuotedSymbol => M2SemanticToken::new(M2SemanticTokenType::EnumMember),
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
    let mut token = knowledge.semantic_token_for_static_type(type_name)?;
    match symbol.meta().symbol_kind {
        _ if token.token_type == M2SemanticTokenType::Class
            && knowledge.has_type_role(&ObjectName::new(type_name), TypeRole::Ring) =>
        {
            token.token_type = M2SemanticTokenType::Type;
            Some(token)
        }
        Some(SymbolKind::VARIABLE)
            if matches!(
                token.token_type,
                M2SemanticTokenType::String
                    | M2SemanticTokenType::Number
                    | M2SemanticTokenType::EnumMember
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

/// Classify one indexed object using its record and the known type lattice.
pub fn semantic_token_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    name: &str,
) -> Option<M2SemanticToken> {
    let record = knowledge.get_record(&ObjectName::new(name))?;
    let data_type = &record.class;
    let subtype_of = |parent: &str| knowledge.is_subtype(data_type, &ObjectName::new(parent));

    let is_command = subtype_of("Command");
    let is_file = subtype_of("File");
    let is_manipulator = subtype_of("Manipulator");
    let is_scripted_functor = subtype_of("ScriptedFunctor");
    let is_compiled_function =
        subtype_of("CompiledFunction") || subtype_of("CompiledFunctionClosure");
    let is_primary_core_compiled_function = data_type.as_ref() == "CompiledFunction"
        && record.name.name() == name
        && knowledge
            .object(&record.package)
            .is_some_and(|package| package.name.name() == "Core");

    if is_primary_core_compiled_function {
        return Some(
            M2SemanticToken::new(M2SemanticTokenType::Function)
                .with_modifier(M2SemanticTokenModifier::Builtin),
        );
    }

    if record_is_type_like(record) {
        return Some(indexed_semantic_token(
            if data_type.as_ref() == "Type" {
                M2SemanticTokenType::Class
            } else {
                M2SemanticTokenType::Type
            },
            false,
            false,
            false,
        ));
    }

    if subtype_of("Function") || is_scripted_functor || is_manipulator || is_command {
        let has_installed_methods = record
            .callable()
            .is_some_and(|info| !info.methods.is_empty());
        let token_type = if is_manipulator {
            M2SemanticTokenType::Operator
        } else if is_command || is_scripted_functor || is_compiled_function {
            M2SemanticTokenType::Function
        } else if has_installed_methods {
            M2SemanticTokenType::Method
        } else {
            M2SemanticTokenType::Function
        };

        Some(indexed_semantic_token(
            token_type,
            is_command,
            false,
            is_manipulator,
        ))
    } else if subtype_of("Package") {
        Some(indexed_semantic_token(
            M2SemanticTokenType::Namespace,
            false,
            false,
            false,
        ))
    } else if (subtype_of("Symbol") || is_file) && !subtype_of("Keyword") && !subtype_of("Operator")
    {
        let is_symbol_class = data_type.as_ref() == "Symbol";
        let token_type = if is_symbol_class {
            M2SemanticTokenType::EnumMember
        } else {
            M2SemanticTokenType::Variable
        };

        Some(indexed_semantic_token(token_type, false, is_file, false))
    } else {
        Some(indexed_semantic_token(
            M2SemanticTokenType::Variable,
            false,
            false,
            false,
        ))
    }
}

/// Classify a source object from its inferred static type.
pub fn semantic_token_for_static_type_from_knowledge(
    knowledge: &(impl TypeKnowledge + ?Sized),
    type_name: &str,
) -> Option<M2SemanticToken> {
    let type_name = ObjectName::new(type_name);
    let subtype_of = |parent: &str| knowledge.is_subtype(&type_name, &ObjectName::new(parent));
    let is_command = subtype_of("Command");
    let is_file = subtype_of("File");
    let is_manipulator = subtype_of("Manipulator");
    let is_type_valued = subtype_of("Type");

    let token_type = if type_name.name().starts_with("MethodFunction") {
        M2SemanticTokenType::Method
    } else if subtype_of("Package") {
        M2SemanticTokenType::Namespace
    } else if is_type_valued {
        if knowledge
            .get_record(&type_name)
            .is_some_and(|record| record.class.as_ref() == "Type")
        {
            M2SemanticTokenType::Class
        } else {
            M2SemanticTokenType::Type
        }
    } else if subtype_of("Function")
        || subtype_of("ScriptedFunctor")
        || is_manipulator
        || is_command
    {
        if is_command {
            M2SemanticTokenType::Function
        } else if is_manipulator {
            M2SemanticTokenType::Operator
        } else {
            M2SemanticTokenType::Function
        }
    } else if is_file {
        M2SemanticTokenType::Variable
    } else if subtype_of("Symbol") {
        M2SemanticTokenType::EnumMember
    } else {
        return None;
    };

    Some(indexed_semantic_token(
        token_type,
        is_command,
        is_file,
        is_manipulator,
    ))
}

fn indexed_semantic_token(
    token_type: M2SemanticTokenType,
    is_command: bool,
    is_file: bool,
    is_manipulator: bool,
) -> M2SemanticToken {
    let mut token = M2SemanticToken::new(token_type);
    if is_command || is_manipulator {
        token = token.with_modifier(M2SemanticTokenModifier::Command);
    }
    if is_file {
        token = token.with_modifier(M2SemanticTokenModifier::File);
    }
    token
}

fn record_is_type_like(record: &Record) -> bool {
    record.type_info().is_some()
}
