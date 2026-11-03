use std::collections::HashSet;
use std::fs;
use std::path::Path;

use m2_syn::nodes::{Expr, ExprOption as OptionExpression, Symbol};
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, CompletionTextEdit,
    Position, Range as TextRange, SymbolKind, TextEdit, Url,
};

use crate::builtin_index::{CallableKind, Record};
use crate::capabilities::document_symbols::WorkspaceSourceSymbol;
use crate::document::DocumentSnapshot;
use crate::file_load::FileLoad;
use crate::macaulay2::Macaulay2Conventions;
use crate::object_registry::{ObjectKnowledge, ObjectName};
use crate::package_index::{is_package_import_trigger, PackageLoadingOption};
use crate::record_lsp::LspKnowledge;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::string_literal::{LiteralSource, StringDelimiter};
use crate::syntax::ExpressionSyntax;
use crate::typesystem::{SubtypeEvidence, TypeKnowledge, TypeRole};

/// Language keywords offered alongside visible symbol completions.
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

/// Maximum number of candidates returned by an ordinary completion query.
const COMPLETION_LIMIT: usize = 80;
/// Maximum candidate count allowed for a short symbol prefix.
const NARROW_COMPLETION_LIMIT: usize = 4;
/// Prefix length below which symbol completion uses the narrow limit.
const NARROW_PREFIX_LENGTH: usize = 2;

/// Object and type queries needed to select completion candidates.
pub trait CompletionKnowledge {
    /// Visible object names matching a prefix up to the requested result count.
    fn records_with_prefix<'a>(&'a self, prefix: &str, limit: usize) -> Vec<(String, &'a Record)>;

    /// Known package names matching a prefix up to the result limit.
    fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;

    /// Looks up the visible catalog record for a completion name.
    fn record<'a>(&'a self, name: &ObjectName) -> Option<&'a Record>;

    /// Available proof that the child type belongs to the parent's subtype family.
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

/// The document, cursor, and recognized syntax for one completion request.
pub struct CompletionContext<'a> {
    /// Immutable document snapshot being queried.
    document: &'a DocumentSnapshot,
    /// Document location associated with the request.
    uri: &'a Url,
    /// LSP UTF-16 position at which the request is evaluated.
    position: Position,
    /// Document UTF-8 byte offset corresponding to the requested LSP position.
    cursor: usize,
    /// Recognized symbol under the cursor, when present.
    symbol: Option<&'a Symbol>,
    /// Entered symbol text and the range completion will replace.
    symbol_prefix: Option<CompletionPrefix>,
    /// Visible definitions found in package sources when no builtin catalog is available.
    source_symbols: &'a [WorkspaceSourceSymbol],
}

/// Recognizes a cursor context and selects completion sources for it.
pub trait CompletionPattern: Sync {
    /// Recognizes this completion pattern at the current cursor position.
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery>;

    /// Characters on which the client should request this completion pattern.
    fn trigger_characters(&self) -> &'static [&'static str] {
        &[]
    }
}

/// Produces candidates for a recognized completion prefix.
pub trait CompletionSource: Send + Sync {
    /// Candidates accepted by this completion source for the entered prefix.
    fn candidates(
        &self,
        context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate>;
}

/// Selected candidate sources, replacement prefix, and result limit.
pub struct CompletionQuery {
    /// Entered text and replacement coordinates used to select candidates.
    prefix: CompletionPrefix,
    /// Candidate providers or source resolver supplying this operation.
    sources: Vec<Box<dyn CompletionSource>>,
    /// Maximum result count or suppression policy for broad candidate sets.
    limit: CompletionLimit,
}

impl CompletionQuery {
    /// Constructs this value from the supplied dependencies and initial state.
    pub fn new(prefix: CompletionPrefix, source: impl CompletionSource + 'static) -> Self {
        Self {
            prefix,
            sources: vec![Box::new(source)],
            limit: CompletionLimit::UpTo(COMPLETION_LIMIT),
        }
    }

    /// Adds another completion source to this query without changing its prefix.
    pub fn and(mut self, source: impl CompletionSource + 'static) -> Self {
        self.sources.push(Box::new(source));
        self
    }

    /// Suppresses this query when its candidate set exceeds the supplied bound.
    pub fn only_when_at_most(mut self, limit: usize) -> Self {
        self.limit = CompletionLimit::AtMost(limit);
        self
    }
}

/// A proposed completion with presentation details and ranking priority.
pub struct CompletionCandidate {
    /// Text presented to the editor or user.
    label: String,
    /// Semantic or presentation category of this item.
    kind: CompletionItemKind,
    /// Additional text shown beside the completion label.
    detail: Option<String>,
    /// Relative candidate ranking, with lower values preferred.
    priority: u8,
}

impl CompletionCandidate {
    /// Constructs this value from the supplied dependencies and initial state.
    pub fn new(label: impl Into<String>, kind: CompletionItemKind) -> Self {
        Self {
            label: label.into(),
            kind,
            detail: None,
            priority: 0,
        }
    }

    /// Attaches explanatory text to a completion candidate.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Sets the ranking priority of this completion candidate.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

/// Text already entered together with the range a completion replaces.
#[derive(Clone)]
pub struct CompletionPrefix {
    /// Original or extracted text represented by this item.
    text: String,
    /// Source range replaced when accepting the completion.
    replace: TextRange,
}

/// Whether to truncate candidates or suppress an overly broad result set.
enum CompletionLimit {
    /// Returns no more than the recorded number of candidates.
    UpTo(usize),
    /// Suppresses completion when more than the recorded number of candidates match.
    AtMost(usize),
}

impl CompletionLimit {
    /// Numeric limit used when requesting candidates from a source.
    fn value(&self) -> usize {
        match self {
            Self::UpTo(limit) | Self::AtMost(limit) => *limit,
        }
    }
}

/// Recognizes the string argument of a package-loading call.
struct PackageImportPattern;
/// Recognizes a file-loading string whose path can be completed.
struct LoadedFilePattern;
/// Recognizes a class argument of a new expression.
struct NewTypePattern;
/// Recognizes a value position in a callable's option assignment.
struct OptionValuePattern;
/// Recognizes an option-name position for a known callable.
struct OptionKeyPattern;
/// Recognizes short symbol prefixes requiring a narrow candidate set.
struct NarrowSymbolPattern;

/// Shared completion recognizer for package import.
static PACKAGE_IMPORT_PATTERN: PackageImportPattern = PackageImportPattern;
/// Shared completion recognizer for loaded file.
static LOADED_FILE_PATTERN: LoadedFilePattern = LoadedFilePattern;
/// Shared completion recognizer for new type.
static NEW_TYPE_PATTERN: NewTypePattern = NewTypePattern;
/// Shared completion recognizer for option value.
static OPTION_VALUE_PATTERN: OptionValuePattern = OptionValuePattern;
/// Shared completion recognizer for option key.
static OPTION_KEY_PATTERN: OptionKeyPattern = OptionKeyPattern;
/// Shared completion recognizer for narrow symbol.
static NARROW_SYMBOL_PATTERN: NarrowSymbolPattern = NarrowSymbolPattern;

/// Completion recognizers evaluated in priority order for each cursor request.
static COMPLETION_PATTERNS: [&dyn CompletionPattern; 6] = [
    &PACKAGE_IMPORT_PATTERN,
    &LOADED_FILE_PATTERN,
    &NEW_TYPE_PATTERN,
    &OPTION_VALUE_PATTERN,
    &OPTION_KEY_PATTERN,
    &NARROW_SYMBOL_PATTERN,
];

/// Protocol completion options including triggers recognized by the supported cursor patterns.
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

/// Recognizes the cursor context and returns ranked completion items with replacement ranges.
/// `source_symbols` supplements builtin metadata with visible source definitions.
pub fn completion_response<Knowledge: LspKnowledge>(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &Knowledge,
    source_symbols: &[WorkspaceSourceSymbol],
) -> Option<CompletionResponse> {
    let mut context = CompletionContext::new(document, uri, position)?;
    context.source_symbols = source_symbols;
    let knowledge: &dyn CompletionKnowledge = knowledge;
    COMPLETION_PATTERNS
        .iter()
        .find_map(|pattern| pattern.query(&context))
        .and_then(|query| complete(query, &context, knowledge))
}

impl<'a> CompletionContext<'a> {
    /// Constructs this value from the supplied dependencies and initial state.
    fn new(document: &'a DocumentSnapshot, uri: &'a Url, position: Position) -> Option<Self> {
        let cursor = document.byte_for_position(position)?;
        let symbol = document.cursor_syntax(position).symbol.or_else(|| {
            let start = cursor.checked_sub(1)?;
            document
                .cursor_syntax(document.position_for_byte(start))
                .symbol
        });
        let symbol_prefix =
            symbol.and_then(|symbol| CompletionPrefix::symbol(document, cursor, symbol));
        Some(Self {
            document,
            uri,
            position,
            cursor,
            symbol,
            symbol_prefix,
            source_symbols: &[],
        })
    }

    /// Recognized symbol syntax associated with this completion request.
    fn symbol_node(&self) -> Option<&'a Symbol> {
        self.symbol
    }

    /// The text of `string` before the cursor, when the cursor is in it.
    fn string_prefix(&self, string: &Expr) -> Option<CompletionPrefix> {
        let start = string.literal_content_bytes(self.document)?.start;
        let end = self.document.syntax_bytes(string)?.end;
        (start <= self.cursor && self.cursor <= end).then(|| CompletionPrefix {
            text: self.document.text()[start..self.cursor].to_string(),
            replace: self.document.range_for_bytes(start..self.cursor),
        })
    }

    /// The string containing the cursor and its enclosing typed expressions.
    fn string_context(&self) -> Option<(&'a Expr, Vec<&'a Expr>)> {
        let byte = self.cursor.checked_sub(1)?;
        let expressions = self
            .document
            .enclosing_expressions(self.document.position_for_byte(byte));
        let string = expressions
            .iter()
            .rev()
            .copied()
            .find(|expression| StringDelimiter::of_expression(expression).is_some())?;
        Some((string, expressions))
    }

    /// Whether a containing application passes the string as its first argument.
    fn string_argument(
        &self,
        string: &Expr,
        expressions: &[&Expr],
        trigger: impl Fn(&str) -> bool,
    ) -> bool {
        expressions.iter().any(|expression| {
            let Expr::Op(m2_syn::nodes::ExprOp::Bin(m2_syn::nodes::ExprOpBin::Adj(call))) =
                expression
            else {
                return false;
            };
            call.left_operand.symbol_name().is_some_and(&trigger)
                && call
                    .right_operand
                    .arguments()
                    .first()
                    .copied()
                    .flatten()
                    .is_some_and(|argument| std::ptr::eq(argument, string))
        })
    }
}

impl CompletionPrefix {
    /// Builds a symbol completion prefix with its exact replacement range.
    fn symbol(
        source: &(impl SourceNavigation + ?Sized),
        cursor: usize,
        symbol: &Symbol,
    ) -> Option<Self> {
        let bytes = source.syntax_bytes(symbol)?;
        let start = bytes.start;
        (start < cursor && cursor <= bytes.end).then(|| Self {
            text: source.text()[start..cursor].to_string(),
            replace: source.range_for_bytes(start..cursor),
        })
    }
}

impl CompletionPattern for PackageImportPattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let (string, expressions) = context.string_context()?;
        let option = expressions.iter().any(|expression| {
            let Expr::Op(m2_syn::nodes::ExprOp::Bin(m2_syn::nodes::ExprOpBin::Adj(call))) =
                expression
            else {
                return false;
            };
            PackageLoadingOption::ALL.into_iter().any(|option| {
                option
                    .typed_package_names(call, context.document)
                    .into_iter()
                    .any(|package| std::ptr::eq(package, string))
            })
        });
        (option || context.string_argument(string, &expressions, is_package_import_trigger))
            .then_some(())?;
        Some(CompletionQuery::new(
            context.string_prefix(string)?,
            PackageNames,
        ))
    }

    fn trigger_characters(&self) -> &'static [&'static str] {
        &["\""]
    }
}

impl CompletionPattern for LoadedFilePattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let (string, expressions) = context.string_context()?;
        context
            .string_argument(string, &expressions, FileLoad::is_load_function)
            .then_some(())?;
        Some(CompletionQuery::new(
            context.string_prefix(string)?,
            LoadableFiles {
                delimiter: StringDelimiter::of_expression(string)?,
            },
        ))
    }

    fn trigger_characters(&self) -> &'static [&'static str] {
        &["\"", "/"]
    }
}

impl CompletionPattern for NewTypePattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        let symbol = context.symbol_node()?;
        let symbol_bytes = context.document.syntax_bytes(symbol)?;
        let new_statement = context
            .document
            .enclosing_expressions(context.document.position_for_byte(symbol_bytes.start))
            .into_iter()
            .rev()
            .find_map(|expression| match expression {
                Expr::New(new) => Some(new),
                _ => None,
            })?;
        let class = context
            .document
            .syntax_bytes(new_statement.class.as_ref())?;
        (class.start <= symbol_bytes.start && symbol_bytes.end <= class.end).then(|| {
            CompletionQuery::new(prefix, KnownSymbols::of_type(TypeRole::Type.object_name()))
        })
    }
}

impl CompletionPattern for OptionValuePattern {
    fn query(&self, context: &CompletionContext<'_>) -> Option<CompletionQuery> {
        let prefix = context.symbol_prefix.clone()?;
        let symbol = context.symbol_node()?;
        let symbol_bytes = context.document.syntax_bytes(symbol)?;
        let option = OptionAtCursor::find(context.document, context.cursor, symbol_bytes.clone())?;
        let value = context.document.syntax_bytes(option.value.as_ref())?;
        (value.start <= symbol_bytes.start && symbol_bytes.end <= value.end).then_some(())?;
        let key = option.key.symbol_name()?;
        let option_bytes = context.document.syntax_bytes(option)?;
        let call = super::signature_help::CallAtCursor::find(
            context.document,
            context.cursor,
            option_bytes,
        )?;
        call.left_operand.symbol_name().map(|callable| {
            CompletionQuery::new(
                prefix,
                OptionValues {
                    callable: ObjectName::new(callable),
                    option: ObjectName::new(key),
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
        let symbol_bytes = context.document.syntax_bytes(symbol)?;
        if OptionAtCursor::find(context.document, context.cursor, symbol_bytes.clone())
            .and_then(|option| context.document.syntax_bytes(option.value.as_ref()))
            .is_some_and(|value| value.start <= symbol_bytes.start && symbol_bytes.end <= value.end)
        {
            return None;
        }
        let call = super::signature_help::CallAtCursor::find(
            context.document,
            context.cursor,
            symbol_bytes,
        )?;
        call.left_operand.symbol_name().map(|callable| {
            CompletionQuery::new(
                prefix,
                CallableOptions {
                    callable: ObjectName::new(callable),
                },
            )
        })
    }
}

/// Finds the innermost typed option containing a completion symbol.
struct OptionAtCursor<'ast, 'source> {
    /// Document supplying revision-specific coordinates.
    document: &'source DocumentSnapshot,
    /// Cursor position in source bytes.
    cursor: usize,
    /// Complete source extent of the symbol being completed.
    symbol: std::ops::Range<usize>,
    /// Innermost containing option found so far.
    option: Option<&'ast OptionExpression>,
}

impl<'ast> OptionAtCursor<'ast, '_> {
    /// Selects an option in the cell containing the cursor.
    fn find(
        document: &'ast DocumentSnapshot,
        cursor: usize,
        symbol: std::ops::Range<usize>,
    ) -> Option<&'ast OptionExpression> {
        let mut query = OptionAtCursor {
            document,
            cursor,
            symbol,
            option: None,
        };
        for cell in document.syntax().cells_intersecting(cursor..cursor) {
            if let Some(cell) = cell.syntax() {
                query.visit_cell(cell);
            }
        }
        query.option
    }
}

impl<'ast> Visit<'ast> for OptionAtCursor<'ast, '_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self
            .document
            .syntax_bytes(expression)
            .is_none_or(|range| self.cursor < range.start || self.cursor > range.end)
        {
            return;
        }
        visit::visit_expr(self, expression);
    }

    fn visit_expr_option(&mut self, option: &'ast OptionExpression) {
        if self
            .document
            .syntax_bytes(option)
            .is_some_and(|range| range.start <= self.symbol.start && self.symbol.end <= range.end)
        {
            self.option = Some(option);
        }
        visit::visit_expr_option(self, option);
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

/// Completion source for visible names, optionally restricted by type.
struct KnownSymbols {
    /// Type constraint used to filter proposed names.
    required_type: Option<ObjectName>,
}

impl KnownSymbols {
    /// Selects known-symbol completion without a type restriction.
    fn any() -> Self {
        Self {
            required_type: None,
        }
    }

    /// Restricts known-symbol completion to values compatible with the supplied type.
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
        let source = context
            .source_symbols
            .iter()
            .filter(|symbol| self.required_type.is_none() && symbol.name.name().starts_with(prefix))
            .map(|symbol| {
                CompletionCandidate::new(symbol.name.name(), completion_item_kind(symbol.kind))
                    .with_priority(2)
            });
        local.chain(external).chain(source).take(limit).collect()
    }
}

/// Whether a visible source binding satisfies the completion type constraint.
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

/// Completion source for package names known to the catalog and source resolver.
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

/// The source files and directories a load can name from what is typed:
/// the entries of the directory the typed name is in, found the way `load`
/// finds a file, each written the way the literal's `delimiter` can hold it.
/// Hidden entries are offered once their leading `.` is typed; symbolic
/// links count as what they point to.
struct LoadableFiles {
    /// Literal delimiter needed to encode inserted string content.
    delimiter: StringDelimiter,
}

impl CompletionSource for LoadableFiles {
    fn candidates(
        &self,
        context: &CompletionContext<'_>,
        prefix: &str,
        limit: usize,
        _knowledge: &dyn CompletionKnowledge,
    ) -> Vec<CompletionCandidate> {
        let (typed_directory, typed_name) =
            prefix.split_at(prefix.rfind('/').map_or(0, |slash| slash + 1));
        let Some(entries) = self
            .delimiter
            .decode(typed_directory)
            .and_then(|directory| FileLoad::first_location_of(&directory, context.uri))
            .and_then(|directory| fs::read_dir(directory).ok())
        else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if name.starts_with('.') && !typed_name.starts_with('.') {
                    return None;
                }
                let (written, kind) = if fs::metadata(entry.path()).ok()?.is_dir() {
                    let written = self
                        .delimiter
                        .encode(&format!("{name}/"))
                        .or_else(|| self.delimiter.encode(&name))?;
                    (written, CompletionItemKind::FOLDER)
                } else if Path::new(&name)
                    .extension()
                    .is_some_and(|extension| extension == Macaulay2Conventions::SOURCE_EXTENSION)
                {
                    (self.delimiter.encode(&name)?, CompletionItemKind::FILE)
                } else {
                    return None;
                };
                Some(CompletionCandidate::new(
                    format!("{typed_directory}{written}"),
                    kind,
                ))
            })
            .take(limit)
            .collect()
    }
}

/// Completion source for option names accepted by a selected callable.
struct CallableOptions {
    /// Callable identity or expression selected for this operation.
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

/// Completion source for the documented values of a selected option.
struct OptionValues {
    /// Callable identity or expression selected for this operation.
    callable: ObjectName,
    /// Name of the callable option being checked.
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

/// Completion source backed by a fixed list of names and one presentation kind.
struct StaticNames {
    /// Names offered by this fixed candidate source.
    names: &'static [&'static str],
    /// Semantic or presentation category of this item.
    kind: CompletionItemKind,
}

impl StaticNames {
    /// Constructs this value from the supplied dependencies and initial state.
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

/// Combines, ranks, and limits candidates into protocol completion items.
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

/// Converts a symbol presentation category to a completion category.
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

/// Completion category appropriate for a catalog object record.
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
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let document =
            DocumentSnapshot::from_text(text.to_string(), &index).expect("fixture should parse");
        let scoped = index.with_source_imports(text);
        let uri = Url::parse("file:///fixture.m2").expect("fixture uri");
        match completion_response(&document, &uri, position, &scoped, &[]) {
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

    #[test]
    fn typed_option_context_tracks_key_and_nested_value() {
        let text = "outer(Strategy => inner(Choice))\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("snapshot");
        for name in ["Strategy", "Choice"] {
            let start = text.find(name).expect("symbol");
            let option = OptionAtCursor::find(&document, start + 2, start..start + name.len())
                .expect("option");
            assert_eq!(option.key.symbol_name(), Some("Strategy"));
            let value = document
                .syntax_bytes(option.value.as_ref())
                .expect("value span");
            assert_eq!(
                value.start <= start && start + name.len() <= value.end,
                name == "Choice"
            );
        }
    }
}
