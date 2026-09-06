use std::collections::HashSet;

use m2_syn::nodes::{ExprNew as NewStatement, ExprOption as OptionExpression, Symbol};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, CompletionTextEdit,
    Position, Range as TextRange, SymbolKind, TextEdit,
};

use crate::builtin_index::{CallableKind, Record};
use crate::document::DocumentSnapshot;
use crate::node_metadata::M2Node;
use crate::object_registry::{ObjectKnowledge, ObjectName};
use crate::package_index::is_package_import_string;
use crate::record_lsp::LspKnowledge;
use crate::source::SourceNavigation;
use crate::typesystem::{SubtypeEvidence, TypeKnowledge, TypeRole};

const COMPLETION_KEYWORDS: &[&str] = &[
    "if",
    "then",
    "else",
    "for",
    "from",
    "to",
    "do",
    "list",
    "while",
    "when",
    "in",
    "of",
    "break",
    "continue",
    "return",
    "try",
    "catch",
    "throw",
    "new",
    "and",
    "or",
    "not",
    "method",
    "true",
    "false",
    "null",
    "symbol",
    "local",
    "global",
    "threadLocal",
];

const COMPLETION_LIMIT: usize = 80;
const NARROW_COMPLETION_LIMIT: usize = 4;
const NARROW_PREFIX_LENGTH: usize = 2;

pub trait CompletionKnowledge {
    fn records_with_prefix<'a>(&'a self, prefix: &str, limit: usize) -> Vec<(String, &'a Record)>;

    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;

    fn record<'a>(&'a self, name: &ObjectName) -> Option<&'a Record>;

    fn subtype_evidence(&self, child: &ObjectName, parent: &ObjectName) -> SubtypeEvidence;
}

impl<Knowledge: LspKnowledge> CompletionKnowledge for Knowledge {
    fn records_with_prefix<'a>(&'a self, prefix: &str, limit: usize) -> Vec<(String, &'a Record)> {
        LspKnowledge::records_with_prefix(self, prefix, limit)
    }

    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        LspKnowledge::package_names_with_prefix(self, prefix, limit)
    }

    fn record<'a>(&'a self, name: &ObjectName) -> Option<&'a Record> {
        ObjectKnowledge::get_record(self, name)
    }

    fn subtype_evidence(&self, child: &ObjectName, parent: &ObjectName) -> SubtypeEvidence {
        TypeKnowledge::subtype_evidence(self, child, parent)
    }
}

pub struct CompletionContext<'a> {
    document: &'a DocumentSnapshot,
    position: Position,
    cursor: usize,
    node: M2Node<'a>,
    symbol: Option<M2Node<'a>>,
    symbol_prefix: Option<CompletionPrefix>,
}

pub trait CompletionPattern: Sync {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery>;

    fn trigger_characters(&self) -> &'static [&'static str] {
        &[]
    }
}

pub trait CompletionSource: Send + Sync {
    fn candidates(
        &self,
        context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate>;
}

pub struct CompletionQuery {
    prefix: CompletionPrefix,
    sources: Vec<Box<dyn CompletionSource>>,
    limit: CompletionLimit,
}

impl CompletionQuery {
    pub fn new(prefix: CompletionPrefix, source: impl CompletionSource + 'static) -> Self {
        Self {
            prefix,
            sources: vec![Box::new(source)],
            limit: CompletionLimit::UpTo(COMPLETION_LIMIT),
        }
    }

    pub fn and(mut self, source: impl CompletionSource + 'static) -> Self {
        self.sources.push(Box::new(source));
        self
    }

    pub fn only_when_at_most(mut self, limit: usize) -> Self {
        self.limit = CompletionLimit::AtMost(limit);
        self
    }
}

pub struct CompletionCandidate {
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
    priority: u8,
}

impl CompletionCandidate {
    pub fn new(label: impl Into<String>, kind: CompletionItemKind) -> Self {
        Self {
            label: label.into(),
            kind,
            detail: None,
            priority: 0,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

#[derive(Clone)]
pub struct CompletionPrefix {
    text: String,
    replace: TextRange,
}

enum CompletionLimit {
    UpTo(usize),
    AtMost(usize),
}

impl CompletionLimit {
    fn value(&self) -> usize {
        match self {
            Self::UpTo(limit) | Self::AtMost(limit) => *limit,
        }
    }
}

struct PackageImportPattern;
struct NewTypePattern;
struct OptionValuePattern;
struct OptionKeyPattern;
struct NarrowSymbolPattern;

static PACKAGE_IMPORT_PATTERN: PackageImportPattern = PackageImportPattern;
static NEW_TYPE_PATTERN: NewTypePattern = NewTypePattern;
static OPTION_VALUE_PATTERN: OptionValuePattern = OptionValuePattern;
static OPTION_KEY_PATTERN: OptionKeyPattern = OptionKeyPattern;
static NARROW_SYMBOL_PATTERN: NarrowSymbolPattern = NarrowSymbolPattern;

static COMPLETION_PATTERNS: [&dyn CompletionPattern; 5] = [
    &PACKAGE_IMPORT_PATTERN,
    &NEW_TYPE_PATTERN,
    &OPTION_VALUE_PATTERN,
    &OPTION_KEY_PATTERN,
    &NARROW_SYMBOL_PATTERN,
];

pub fn completion_options() -> CompletionOptions {
    let mut seen = HashSet::new();
    let trigger_characters = COMPLETION_PATTERNS
        .iter()
        .flat_map(|pattern| pattern.trigger_characters())
        .filter(|trigger| seen.insert(**trigger))
        .map(|trigger| (*trigger).to_string())
        .collect();
    CompletionOptions {
        trigger_characters: Some(trigger_characters),
        ..Default::default()
    }
}

pub fn completion_response<Knowledge: LspKnowledge>(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &Knowledge,
) -> Option<CompletionResponse> {
    let context = CompletionContext::new(document, position)?;
    let knowledge: &dyn CompletionKnowledge = knowledge;
    COMPLETION_PATTERNS
        .iter()
        .find_map(|pattern| pattern.query(&context))
        .and_then(|query| complete(query, &context, knowledge))
}

impl<'a> CompletionContext<'a> {
    fn new(document: &'a DocumentSnapshot, position: Position) -> Option<Self> {
        let cursor = document.byte_for_position(position)?;
        let node = document.node_at_position_minimal(position)?;
        let symbol = node
            .enclosing_node(|candidate| candidate.is::<Symbol>())
            .or_else(|| {
                let start = cursor.checked_sub(1)?;
                document
                    .root_node()
                    .descendant_for_point_range(
                        document.point_for_byte(start),
                        document.point_for_byte(cursor),
                    )?
                    .enclosing_node(|candidate| candidate.is::<Symbol>())
            });
        let symbol_prefix =
            symbol.and_then(|symbol| CompletionPrefix::symbol(document, cursor, symbol));
        Some(Self {
            document,
            position,
            cursor,
            node,
            symbol,
            symbol_prefix,
        })
    }

    fn symbol_node(&self) -> Option<M2Node<'a>> {
        self.symbol
    }

    fn package_prefix(&self, string: M2Node<'_>) -> Option<CompletionPrefix> {
        let opening = string.child(0)?;
        let start = opening.end_byte();
        (start <= self.cursor && self.cursor <= string.end_byte()).then(|| CompletionPrefix {
            text: self.document.text()[start..self.cursor].to_string(),
            replace: self.document.range_for_bytes(start..self.cursor),
        })
    }
}

impl CompletionPrefix {
    fn symbol(
        source: &(impl SourceNavigation + ?Sized),
        cursor: usize,
        symbol: M2Node<'_>,
    ) -> Option<Self> {
        let start = symbol.start_byte();
        (start < cursor && cursor <= symbol.end_byte()).then(|| Self {
            text: source.text()[start..cursor].to_string(),
            replace: source.range_for_bytes(start..cursor),
        })
    }
}

impl CompletionPattern for PackageImportPattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let string = context
            .node
            .enclosing_node(|node| node.is_string_literal())?;
        is_package_import_string(string).then_some(())?;
        Some(CompletionQuery::new(
            context.package_prefix(string)?,
            PackageNames,
        ))
    }

    fn trigger_characters(&self) -> &'static [&'static str] {
        &["\""]
    }
}

impl CompletionPattern for NewTypePattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        let symbol = context.symbol_node()?;
        let new_statement = symbol.enclosing_node(|node| node.is::<NewStatement>())?;
        let class = new_statement.child_by_field_name("type")?;
        class.contains(symbol).then(|| {
            CompletionQuery::new(prefix, KnownSymbols::of_type(TypeRole::Type.object_name()))
        })
    }
}

impl CompletionPattern for OptionValuePattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        let symbol = context.symbol_node()?;
        let option = symbol.enclosing_node(|node| node.is::<OptionExpression>())?;
        option
            .child_by_field_name("right")?
            .contains(symbol)
            .then_some(())?;
        let key = option.child_by_field_name("left")?;
        key.is::<Symbol>().then_some(())?;
        let (callable, _) = super::signature_help::enclosing_application(option, context.cursor)?;
        callable.is::<Symbol>().then(|| {
            CompletionQuery::new(
                prefix,
                OptionValues {
                    callable: ObjectName::new(callable.text()),
                    option: ObjectName::new(key.text()),
                },
            )
        })
    }

    fn trigger_characters(&self) -> &'static [&'static str] {
        &[">"]
    }
}

impl CompletionPattern for OptionKeyPattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        prefix.text.chars().next()?.is_uppercase().then_some(())?;
        let symbol = context.symbol_node()?;
        if symbol
            .enclosing_node(|node| node.is::<OptionExpression>())
            .and_then(|option| option.child_by_field_name("right"))
            .is_some_and(|value| value.contains(symbol))
        {
            return None;
        }
        let (callable, _) = super::signature_help::enclosing_application(symbol, context.cursor)?;
        callable.is::<Symbol>().then(|| {
            CompletionQuery::new(
                prefix,
                CallableOptions {
                    callable: ObjectName::new(callable.text()),
                },
            )
        })
    }
}

impl CompletionPattern for NarrowSymbolPattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        (prefix.text.chars().count() >= NARROW_PREFIX_LENGTH).then_some(())?;
        context.symbol_node()?;
        Some(
            CompletionQuery::new(prefix, KnownSymbols::any())
                .and(StaticNames::new(
                    COMPLETION_KEYWORDS,
                    CompletionItemKind::KEYWORD,
                ))
                .only_when_at_most(NARROW_COMPLETION_LIMIT),
        )
    }
}

struct KnownSymbols {
    required_type: Option<ObjectName>,
}

impl KnownSymbols {
    fn any() -> Self {
        Self {
            required_type: None,
        }
    }

    fn of_type(required_type: ObjectName) -> Self {
        Self {
            required_type: Some(required_type),
        }
    }
}

impl CompletionSource for KnownSymbols {
    fn candidates(
        &self,
        context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        let local = context
            .document
            .analysis()
            .in_scope_bindings(prefix, context.position)
            .into_iter()
            .filter(|binding| {
                self.required_type.as_ref().is_none_or(|required_type| {
                    binding_matches_type(*binding, required_type, knowledge)
                })
            })
            .map(|binding| {
                let mut candidate = CompletionCandidate::new(
                    binding.name.name(),
                    completion_item_kind(binding.state.presentation_kind),
                );
                if let Some(detail) = binding
                    .state
                    .inferred_type
                    .as_ref()
                    .and_then(|inferred| inferred.label())
                {
                    candidate = candidate.with_detail(detail);
                }
                candidate
            });
        let external = knowledge
            .records_with_prefix(prefix, usize::MAX)
            .into_iter()
            .filter(|(_, record)| {
                self.required_type.as_ref().is_none_or(|required_type| {
                    knowledge.subtype_evidence(&record.class, required_type)
                        == SubtypeEvidence::Proven
                })
            })
            .map(|(package, record)| {
                let detail = if package == "Core" {
                    record.class.name().to_string()
                } else {
                    format!("{} from {package}", record.class.name())
                };
                CompletionCandidate::new(record.name.name(), record_completion_kind(record))
                    .with_detail(detail)
                    .with_priority(2)
            });
        local.chain(external).take(limit).collect()
    }
}

fn binding_matches_type(
    binding: crate::analysis::BindingView<'_>,
    required_type: &ObjectName,
    knowledge: &dyn CompletionKnowledge,
) -> bool {
    if required_type == &TypeRole::Type.object_name() && binding.state.source_type.is_some() {
        return true;
    }
    let Some(inferred) = &binding.state.inferred_type else {
        return false;
    };
    let mut inferred_types = inferred.exact_points().chain(inferred.upward_generators());
    let Some(first) = inferred_types.next() else {
        return false;
    };
    std::iter::once(first)
        .chain(inferred_types)
        .all(|inferred_type| {
            knowledge.subtype_evidence(inferred_type, required_type) == SubtypeEvidence::Proven
        })
}

struct PackageNames;

impl CompletionSource for PackageNames {
    fn candidates(
        &self,
        _context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        knowledge
            .package_names_with_prefix(prefix, limit)
            .into_iter()
            .map(|name| {
                CompletionCandidate::new(name, CompletionItemKind::MODULE)
                    .with_detail("Macaulay2 package")
            })
            .collect()
    }
}

struct CallableOptions {
    callable: ObjectName,
}

impl CompletionSource for CallableOptions {
    fn candidates(
        &self,
        _context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        knowledge
            .record(&self.callable)
            .and_then(Record::callable)
            .into_iter()
            .flat_map(|callable| &callable.options)
            .filter(|option| option.name.name().starts_with(prefix))
            .map(|option| {
                CompletionCandidate::new(option.name.name(), CompletionItemKind::PROPERTY)
                    .with_detail(format!("Option for {}", self.callable))
            })
            .take(limit)
            .collect()
    }
}

struct OptionValues {
    callable: ObjectName,
    option: ObjectName,
}

impl CompletionSource for OptionValues {
    fn candidates(
        &self,
        _context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        knowledge
            .record(&self.callable)
            .and_then(Record::callable)
            .into_iter()
            .flat_map(|callable| &callable.options)
            .find(|option| option.name == self.option)
            .into_iter()
            .flat_map(|option| &option.possible_values)
            .filter(|value| value.name().starts_with(prefix))
            .map(|value| {
                let kind = knowledge
                    .record(value)
                    .map_or(CompletionItemKind::VALUE, record_completion_kind);
                CompletionCandidate::new(value.name(), kind)
                    .with_detail(format!("Value for {}.{}", self.callable, self.option))
            })
            .take(limit)
            .collect()
    }
}

struct StaticNames {
    names: &'static [&'static str],
    kind: CompletionItemKind,
}

impl StaticNames {
    fn new(names: &'static [&'static str], kind: CompletionItemKind) -> Self {
        Self { names, kind }
    }
}

impl CompletionSource for StaticNames {
    fn candidates(
        &self,
        _context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        _knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        self.names
            .iter()
            .filter(|name| name.starts_with(prefix))
            .map(|name| CompletionCandidate::new(*name, self.kind).with_priority(1))
            .take(limit)
            .collect()
    }
}

fn complete(
    query: CompletionQuery,
    context: &CompletionContext<'_>,
    knowledge: &dyn CompletionKnowledge,
) -> Option<CompletionResponse> {
    let limit = query.limit.value();
    let fetch_limit = limit.saturating_add(1);
    let mut seen = HashSet::new();
    let mut candidates = query
        .sources
        .iter()
        .flat_map(|source| source.candidates(context, &query.prefix.text, fetch_limit, knowledge))
        .filter(|candidate| candidate.label.starts_with(&query.prefix.text))
        .filter(|candidate| seen.insert(candidate.label.clone()))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.label.cmp(&right.label))
    });
    if matches!(query.limit, CompletionLimit::AtMost(_)) && candidates.len() > limit {
        return None;
    }
    let is_incomplete = candidates.len() > limit;
    candidates.truncate(limit);
    let items = candidates
        .into_iter()
        .map(|candidate| CompletionItem {
            label: candidate.label.clone(),
            kind: Some(candidate.kind),
            detail: candidate.detail,
            sort_text: Some(format!("{:02}-{}", candidate.priority, candidate.label)),
            filter_text: Some(candidate.label.clone()),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                query.prefix.replace,
                candidate.label,
            ))),
            ..Default::default()
        })
        .collect();
    if is_incomplete {
        Some(CompletionResponse::List(
            tower_lsp::lsp_types::CompletionList {
                is_incomplete,
                items,
            },
        ))
    } else {
        Some(CompletionResponse::Array(items))
    }
}

fn completion_item_kind(kind: SymbolKind) -> CompletionItemKind {
    if kind == SymbolKind::FUNCTION {
        CompletionItemKind::FUNCTION
    } else if kind == SymbolKind::METHOD {
        CompletionItemKind::METHOD
    } else if kind == SymbolKind::CLASS {
        CompletionItemKind::CLASS
    } else if kind == SymbolKind::CONSTANT {
        CompletionItemKind::CONSTANT
    } else {
        CompletionItemKind::VARIABLE
    }
}

fn record_completion_kind(record: &Record) -> CompletionItemKind {
    if record.type_info().is_some() {
        return CompletionItemKind::CLASS;
    }
    if let Some(callable) = record.callable() {
        return match &callable.kind {
            CallableKind::MethodFunction => CompletionItemKind::METHOD,
            CallableKind::Function => CompletionItemKind::FUNCTION,
            CallableKind::Operator(_) => CompletionItemKind::OPERATOR,
        };
    }
    match record.class.name() {
        "Package" => CompletionItemKind::MODULE,
        "Symbol" => CompletionItemKind::CONSTANT,
        _ => CompletionItemKind::VALUE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn completion_items(text: &str, position: Position) -> Vec<CompletionItem> {
        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document =
            DocumentSnapshot::from_text(text.to_string(), &index).expect("fixture should parse");
        let scoped = index.with_source_imports(text);
        match completion_response(&document, position, &scoped) {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        }
    }

    fn completion_labels(text: &str, position: Position) -> Vec<String> {
        completion_items(text, position)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn narrow_symbol_completion_stays_bounded() {
        let labels = completion_labels("Loc\n", pos!(0, 3));
        assert_eq!(labels, vec!["Local", "LocalDictionary"]);

        let labels = completion_labels(
            "candidateOne = 1\ncandidateTwo = 2\ncandidateThree = 3\ncandidateFour = 4\ncandidateFive = 5\nca\n",
            pos!(5, 2),
        );
        assert!(labels.is_empty());
    }

    #[test]
    fn new_completion_filters_known_symbols_by_type() {
        let labels = completion_labels("LocalType = new Type\nvalue = new Loc\n", pos!(1, 15));
        assert_eq!(labels, vec!["LocalType", "LocalDictionary"]);
    }

    #[test]
    fn completion_items_replace_the_typed_prefix() {
        let items = completion_items("Loc\n", pos!(0, 3));
        assert!(items.iter().all(|item| {
            matches!(
                &item.text_edit,
                Some(CompletionTextEdit::Edit(edit))
                    if edit.range == TextRange::new(pos!(0, 0), pos!(0, 3))
            )
        }));
    }
}
