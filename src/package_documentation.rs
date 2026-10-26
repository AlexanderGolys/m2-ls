//! Package documentation: the help pages a package writes in its source, in
//! either of the two formats Macaulay2 accepts.
//!
//! - **SimpleDoc**, a string passed to `doc` (or to `document`, or the
//!   deprecated `multidoc`): an off-side-rule outline of section keywords
//!   (`Key`, `Headline`, `Usage`, `Inputs`, `Description`, …) whose indented
//!   lines hold each section's content. [`simple_doc`] reads it exactly as the
//!   `SimpleDoc` package does.
//! - **Hypertext lists**, the older format: a list passed to `document`,
//!   holding `Key => …`, `Headline => …`, and the other documentation options
//!   as options, and the description as hypertext — strings and markup such as
//!   `PARA`, `TO`, and `EXAMPLE`. [`hypertext`] reads it from the syntax tree.
//!
//! Both produce the same facts, so every consumer handles one format-free
//! model:
//!
//! - one [`DocumentationNode`] per help page, with the keys it documents, its
//!   content rendered as Markdown for hover, and the code of its examples;
//! - the keys `undocumented` exempts from documentation;
//! - the names the documentation evaluates in the package — keys, links,
//!   `SeeAlso` entries, input types — which navigation follows and the
//!   diagnostics check;
//! - [`DocumentationMarkup`] spans that highlight the section keywords of
//!   SimpleDoc strings and the code documentation strings embed, token by
//!   token;
//! - [`DocumentationFault`]s: what `document` or `SimpleDoc` reject or warn
//!   about, which the diagnostics report.
//!
//! The examples of a node run in a separate session that has loaded the
//! package, one after another, when `installPackage` builds the
//! documentation. [`PackageDocumentation::example_sessions`] hands each
//! node's example code to the document as one [`PackageSessionCode`], which
//! is analyzed like a TEST block.

mod hypertext;
mod simple_doc;

use m2_syn::nodes::{Array, Sequence, Symbol};
use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::Range as TextRange;

use crate::analysis::Analysis;
use crate::documentation::{DocumentationBlock, DocumentationTarget};
use crate::macaulay2::Macaulay2Conventions;
use crate::meta::BindingRole;
use crate::node_metadata::{SyntaxNode, SyntaxParser};
use crate::object_registry::ObjectName;
use crate::semantic_token::{syntax_semantic_token_type, M2SemanticTokenType};
use crate::source::{ByteRange, DocumentSpan, SourceNavigation, SymbolMention};
use crate::source_package::{CodeFragment, PackageSessionCode, SourcePackages};

pub use hypertext::DocumentOption;
use hypertext::HypertextMarkdown;
pub use simple_doc::SimpleDocKeyword;

/// The functions that read SimpleDoc or hypertext documentation, as they are
/// called in package source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, Display)]
#[strum(serialize_all = "camelCase")]
enum DocumentationFunction {
    /// `doc ///…///`: SimpleDoc.
    Doc,
    /// `multidoc ///…///`: the deprecated synonym of `doc`.
    Multidoc,
    /// `document {…}` for a hypertext list, `document ///…///` for SimpleDoc.
    Document,
    /// `undocumented {…}`: keys exempt from documentation.
    Undocumented,
}

/// Everything the documentation of one source file states.
#[derive(Debug, Default)]
pub struct PackageDocumentation {
    nodes: Vec<DocumentationNode>,
    undocumented: Vec<DocumentationKey>,
    referenced_names: Vec<ReferencedName>,
    /// Strings naming a documentation key by a symbol's name, such as `"f"`
    /// in `TO "f"`: names, but not ones the documentation evaluates.
    key_strings: Vec<DocumentSpan>,
    markup: Vec<DocumentationMarkup>,
    documentation_ranges: Vec<ByteRange>,
    example_ranges: Vec<ByteRange>,
    faults: Vec<DocumentationFault>,
    example_sessions: Vec<PackageSessionCode>,
    simple_doc_functions: Vec<TextRange>,
}

/// One help page.
#[derive(Debug, Clone)]
pub struct DocumentationNode {
    /// The source writing the page: the `document` call of a hypertext list,
    /// the `Node` section of a SimpleDoc string listing several nodes, or the
    /// `doc` call of a SimpleDoc string holding one.
    pub range: TextRange,
    /// The keys the page documents, in source order: the first is the primary
    /// key, the others point to it.
    pub keys: Vec<DocumentationKey>,
    /// The page's content as Markdown.
    pub markdown: String,
    /// The page's example code, in the order the examples run.
    pub examples: Vec<CodeFragment>,
}

/// One key a page documents or `undocumented` exempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationKey {
    /// Where the key is written.
    pub range: TextRange,
    /// What the key names.
    pub subject: DocumentedSubject,
}

/// What a documentation key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentedSubject {
    /// A symbol, written as itself, `f`, or as its name, `"f"`.
    Name(ObjectName),
    /// A method, written as the sequence `(f, ZZ, QQ)` of the method
    /// function and the types it is installed on.
    Method {
        function: ObjectName,
        domain: Vec<ObjectName>,
    },
    /// An optional argument of a function, written as the array `[f, Limit]`
    /// of the function and the option's symbol.
    OptionalArgument {
        function: ObjectName,
        option: ObjectName,
    },
    /// Anything else, such as an overview page titled by a string with spaces
    /// (`"Using Graphs"`) or an operator installation (`(symbol +, ZZ, ZZ)`),
    /// kept as its Macaulay2 source.
    Expression(String),
}

/// How Macaulay2 identifies a documentation key: two keys with the same tag
/// document the same page. A symbol and a string naming it share a tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocumentTag(String);

/// A name documentation evaluates in the package: a symbol in a key, a link, a
/// `SeeAlso` entry, or an input type.
#[derive(Debug, Clone)]
pub struct ReferencedName {
    /// The name as written.
    pub span: DocumentSpan,
    /// Whether the name is written in code or inside a SimpleDoc string.
    pub site: NameSite,
}

/// Where a [`ReferencedName`] is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSite {
    /// In code, such as a hypertext list: the analysis already knows it.
    Code,
    /// Inside a SimpleDoc string: only the documentation knows it is a name.
    SimpleDoc,
}

/// A span of a documentation string that is not prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationMarkup {
    /// The bytes of the span.
    pub bytes: ByteRange,
    /// What the span is.
    pub kind: DocumentationMarkupKind,
}

/// What a [`DocumentationMarkup`] span is. The code a SimpleDoc string
/// embeds — the expressions it evaluates in the package, such as keys,
/// `@TO f@` links, and `SeeAlso` entries, and the code it only shows, its
/// `Usage` lines and `CannedExample`s — is marked token by token, so it reads
/// as the code it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentationMarkupKind {
    /// A section keyword, such as `Headline` or `Example`.
    SectionKeyword,
    /// A name, highlighted as what it names: a symbol of embedded code, or a
    /// string naming a documentation key by a symbol's name, the `f` of
    /// `TO "f"`.
    Name,
    /// The option a key or a link names an optional argument by, the
    /// `Limit` of `[f, Limit]`.
    OptionName,
    /// The name an `Inputs` or `Outputs` item gives the argument or value it
    /// describes, the `n` of `n:ZZ`.
    ItemName,
    /// Markup around documentation text: a delimiter of TeX markup, the
    /// `{\tt` and `}` of `{\tt x}`, or a hypertext constructor such as `TT`
    /// or `PARA`.
    TextMarkup,
    /// A hypertext constructor linking to another documentation page: `TO`,
    /// `TOH`, or `TO2`.
    LinkConstructor,
    /// Documentation text shown in a style: the content of TeX or hypertext
    /// markup, a preformatted block, or a page's headline.
    Styled(TextStyle),
    /// A literal, operator, or keyword of embedded code, highlighted as its
    /// syntax.
    Syntax(M2SemanticTokenType),
}

/// How documentation shows a span of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextStyle {
    /// Bold: `{\bf …}`, a headline.
    Strong,
    /// Emphasized or italic: `{\em …}`, `{\it …}`.
    Emphasis,
    /// Monospace: `{\tt …}`, code and preformatted hypertext, a `Pre`
    /// block.
    Monospace,
    /// The address of a link, `\url{…}`.
    Link,
    /// TeX mathematics, `$…$`.
    Math,
}

/// Something about documentation that `document` or `SimpleDoc` rejects or
/// warns about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationFault {
    /// The source at fault.
    pub range: TextRange,
    /// What is wrong.
    pub kind: DocumentationFaultKind,
}

/// The kinds of [`DocumentationFault`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentationFaultKind {
    /// A SimpleDoc keyword that is not one of `expected`, where only those may
    /// start a section. A content line indented no deeper than the keyword
    /// above it reads as a keyword too.
    UnknownSimpleDocKeyword {
        keyword: String,
        expected: Vec<SimpleDocKeyword>,
    },
    /// An option of a hypertext list that is not a documentation option.
    UnknownDocumentOption { option: String },
    /// A section given twice in one page.
    RepeatedSection { section: String },
    /// A section missing the content it needs, described by `expectation`.
    EmptySection {
        section: String,
        expectation: &'static str,
    },
    /// A `Headline` or `Heading` continued on further lines.
    MultilineHeadline { section: String },
    /// An `Inputs`/`Outputs` item whose first line does not name the item and
    /// its type around one `:` or `=>`.
    MalformedItem,
    /// An `@` opening a Macaulay2 expression in SimpleDoc text that no `@`
    /// closes; write `\@` for a literal `@`.
    UnmatchedAt,
    /// A Macaulay2 expression in SimpleDoc that does not parse.
    MalformedExpression,
    /// A SimpleDoc string mixing `Node` sections with the sections of a page.
    MixedNodes,
    /// Documentation options passed to `document` outside of its list.
    OptionsOutsideList,
    /// A documentation option whose value must be a list.
    ExpectedList { option: String },
    /// `Inputs` or `Outputs` without the `Usage` they describe.
    UsageRequired,
    /// A page without a `Key`.
    MissingKey,
    /// A headline longer than the 100 characters Macaulay2 allows; it rejects
    /// one longer than 200 outright.
    HeadlineTooLong { length: usize },
}

impl PackageDocumentation {
    /// The documentation written in the syntax tree under `root`, a file that
    /// declares `packages`, as `analysis` characterized it.
    pub fn collect(
        root: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
        packages: &SourcePackages,
    ) -> Self {
        let mut collector = DocumentationCollector {
            source,
            parser: SyntaxParser::new().expect("the Macaulay2 grammar loads"),
            documentation: Self::default(),
            evaluated_examples: Vec::new(),
        };
        for call in root.descendants() {
            let Some(function) = call
                .applied_symbol_name()
                .and_then(|name| name.parse::<DocumentationFunction>().ok())
            else {
                continue;
            };
            let Some(argument) = call.binary_parts().map(|parts| parts.right) else {
                continue;
            };
            collector.read_call(call, function, argument);
        }
        let mut documentation = collector.documentation;
        for fragment in collector.evaluated_examples {
            if let Some(node) = documentation.nodes.iter_mut().find(|node| {
                node.range.start <= fragment.range.start && fragment.range.end <= node.range.end
            }) {
                node.examples.push(fragment);
            }
        }
        for node in &mut documentation.nodes {
            node.examples.sort_by_key(|fragment| fragment.bytes.start);
            node.examples
                .dedup_by_key(|fragment| fragment.bytes.clone());
        }
        documentation.example_sessions = documentation
            .nodes
            .iter()
            .filter(|node| !node.examples.is_empty())
            .filter_map(|node| {
                Some(PackageSessionCode {
                    package: packages.session_package_at(node.range.start)?,
                    fragments: node.examples.clone(),
                })
            })
            .collect();
        documentation
    }

    /// Source regions occupied by documentation calls.
    pub fn documentation_ranges(&self) -> &[ByteRange] {
        &self.documentation_ranges
    }

    /// Regions shown or executed as examples, including canned examples.
    pub fn example_ranges(&self) -> &[ByteRange] {
        &self.example_ranges
    }

    /// Every page, in source order.
    pub fn nodes(&self) -> &[DocumentationNode] {
        &self.nodes
    }

    /// The keys `undocumented` exempts from documentation.
    pub fn undocumented(&self) -> &[DocumentationKey] {
        &self.undocumented
    }

    /// The names the documentation evaluates in the package, in source order.
    pub fn referenced_names(&self) -> &[ReferencedName] {
        &self.referenced_names
    }

    /// The keyword and code spans of documentation strings.
    pub fn markup(&self) -> &[DocumentationMarkup] {
        &self.markup
    }

    /// What `document` and `SimpleDoc` reject or warn about.
    pub fn faults(&self) -> &[DocumentationFault] {
        &self.faults
    }

    /// Where the functions the SimpleDoc package defines, `doc` and
    /// `multidoc`, are called, which needs the package loaded.
    pub fn simple_doc_functions(&self) -> &[TextRange] {
        &self.simple_doc_functions
    }

    /// The names only the documentation knows to be names — those SimpleDoc
    /// strings evaluate, and the strings naming a key by a symbol's name:
    /// hover and navigation follow them as symbol mentions.
    pub fn mentions(&self) -> impl Iterator<Item = SymbolMention> + '_ {
        self.referenced_names
            .iter()
            .filter(|name| name.site == NameSite::SimpleDoc)
            .map(|name| &name.span)
            .chain(&self.key_strings)
            .map(|span| SymbolMention::new(span.clone()))
    }

    /// Each page's example code as the session `installPackage` runs it in.
    pub fn example_sessions(&self) -> impl Iterator<Item = &PackageSessionCode> {
        self.example_sessions.iter()
    }

    /// The pages as hover documentation of what their primary keys name in
    /// the document: a binding of the documented symbol, or a method
    /// installation matching the documented method.
    pub fn documentation_blocks(&self, analysis: &Analysis) -> Vec<DocumentationBlock> {
        self.nodes
            .iter()
            .filter_map(|node| {
                let key = node.keys.first()?;
                let targets = key.subject.targets(analysis);
                (!targets.is_empty())
                    .then(|| DocumentationBlock::new(node.markdown.clone(), targets))
            })
            .collect()
    }
}

impl DocumentedSubject {
    /// What a key written as `expression` names.
    fn of_expression(expression: SyntaxNode<'_>) -> Self {
        if expression.is::<Symbol>() {
            return Self::Name(ObjectName::new(expression.text()));
        }
        if let Some(name) = expression.string_literal_inner_text() {
            return Self::Name(ObjectName::new(name));
        }
        if let Some((function, option)) = Self::optional_argument_parts(expression) {
            return Self::OptionalArgument {
                function: ObjectName::new(function.text()),
                option: ObjectName::new(option.text()),
            };
        }
        if expression.is::<Sequence>() {
            let mut elements = expression.collection_elements();
            let function = elements.next().filter(|function| function.is::<Symbol>());
            let domain = elements.collect::<Vec<_>>();
            if let Some(function) = function {
                if !domain.is_empty() && domain.iter().all(|parameter| parameter.is::<Symbol>()) {
                    return Self::Method {
                        function: ObjectName::new(function.text()),
                        domain: domain
                            .iter()
                            .map(|parameter| ObjectName::new(parameter.text()))
                            .collect(),
                    };
                }
            }
        }
        Self::Expression(expression.text().split_whitespace().collect())
    }

    /// The function and the option of `expression` when it is a key of an
    /// optional argument, the array `[f, Limit]`.
    fn optional_argument_parts(
        expression: SyntaxNode<'_>,
    ) -> Option<(SyntaxNode<'_>, SyntaxNode<'_>)> {
        if !expression.is::<Array>() {
            return None;
        }
        match expression.collection_elements().collect::<Vec<_>>()[..] {
            [function, option] if function.is::<Symbol>() && option.is::<Symbol>() => {
                Some((function, option))
            }
            _ => None,
        }
    }

    /// The tag Macaulay2 files the documentation of the subject under.
    pub fn tag(&self) -> DocumentTag {
        DocumentTag(match self {
            Self::Name(name) => name.name().to_string(),
            Self::Method { function, domain } => format!(
                "{}({})",
                function.name(),
                domain
                    .iter()
                    .map(ObjectName::name)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::OptionalArgument { function, option } => {
                format!("[{},{}]", function.name(), option.name())
            }
            Self::Expression(source) => source.clone(),
        })
    }

    /// The name hover finds the subject's page under: the symbol, the
    /// method function, or the option's symbol.
    pub fn documented_name(&self) -> Option<&ObjectName> {
        match self {
            Self::Name(name) => Some(name),
            Self::Method { function, .. } => Some(function),
            Self::OptionalArgument { option, .. } => Some(option),
            Self::Expression(_) => None,
        }
    }

    /// What the subject names among the definitions `analysis` found: the
    /// global bindings of a documented symbol, or the installations of a
    /// documented method on exactly its domain.
    fn targets(&self, analysis: &Analysis) -> Vec<DocumentationTarget> {
        match self {
            Self::Name(name) => analysis
                .binding_states()
                .filter(|binding| {
                    binding.role == BindingRole::Ordinary
                        && binding.state.scope_idx == 0
                        && binding.name == *name
                })
                .map(|binding| DocumentationTarget::Binding {
                    binding_id: binding.binding_id,
                    state_span: binding.state.span,
                    name: binding.name.clone(),
                })
                .collect(),
            Self::Method { function, domain } => analysis
                .installations()
                .iter()
                .filter(|installation| {
                    installation.method.head.name() == function
                        && installation.method.domain == *domain
                })
                .map(|installation| DocumentationTarget::Installation {
                    installation_id: installation.id,
                    name: function.clone(),
                    anchor: installation.span,
                })
                .collect(),
            Self::OptionalArgument { .. } | Self::Expression(_) => Vec::new(),
        }
    }
}

/// The content of one page, assembled section by section into the Markdown
/// hover shows, laid out as the pages of the documentation Macaulay2 installs
/// are: the headline, usage, inputs, optional arguments, outputs,
/// consequences, the description, the subsections such as `Caveat`, the menu
/// of subnodes, and the keys it links to.
#[derive(Debug, Default)]
struct NodeMarkdown {
    headline: Option<String>,
    usage: Vec<String>,
    inputs: Vec<String>,
    /// The `Inputs` items naming an optional argument, `Opt => …`.
    options: Vec<String>,
    outputs: Vec<String>,
    consequences: Vec<String>,
    /// Markdown blocks: paragraphs, examples, menus.
    description: Vec<String>,
    /// Each subsection's title and Markdown content.
    subsections: Vec<(String, String)>,
    /// Markdown list items, nested by indentation.
    subnodes: Vec<String>,
    /// The rendered links.
    see_also: Vec<String>,
}

impl NodeMarkdown {
    /// The page as one Markdown document.
    fn render(&self) -> String {
        let list = |items: &[String]| {
            items
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut blocks = Vec::new();
        if let Some(headline) = &self.headline {
            blocks.push(headline.clone());
        }
        if !self.usage.is_empty() {
            blocks.push(format!(
                "## Usage\n\n{}",
                Self::fenced_code(
                    Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE,
                    &self.usage.join("\n")
                )
            ));
        }
        for (title, items) in [
            ("Parameters", &self.inputs),
            ("Options", &self.options),
            ("Returns", &self.outputs),
            ("Consequences", &self.consequences),
        ] {
            if !items.is_empty() {
                blocks.push(format!("## {title}\n\n{}", list(items)));
            }
        }
        let description = self
            .description
            .iter()
            .map(|block| block.trim())
            .filter(|block| !block.is_empty())
            .collect::<Vec<_>>();
        if !description.is_empty() {
            blocks.push(format!("## Description\n\n{}", description.join("\n\n")));
        }
        for (title, content) in &self.subsections {
            blocks.push(format!("## {title}\n\n{content}"));
        }
        if !self.subnodes.is_empty() {
            blocks.push(format!("## Menu\n\n{}", self.subnodes.join("\n")));
        }
        if !self.see_also.is_empty() {
            blocks.push(format!("## See also\n\n{}", list(&self.see_also)));
        }
        blocks.join("\n\n")
    }

    /// One parameter, option, or return value as a Markdown list item, as the
    /// installed documentation writes it: `**name** — type, description`,
    /// its `name` rendered, the empty parts of `description` left out.
    fn item<const PARTS: usize>(name: Option<&str>, description: [String; PARTS]) -> String {
        let description = description
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        match (name, description.is_empty()) {
            (Some(name), true) => format!("**{name}**"),
            (Some(name), false) => format!("**{name}** — {description}"),
            (None, _) => description,
        }
    }

    /// `code` as a fenced Markdown code block in `language`.
    fn fenced_code(language: &str, code: &str) -> String {
        format!("```{language}\n{code}\n```")
    }
}

impl DocumentTag {
    /// The tag as Macaulay2 formats it in messages.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reads the documentation calls of one file into [`PackageDocumentation`].
///
/// The format readers, [`simple_doc`] and [`hypertext`], extend it with the
/// methods for their format; the methods here are what both share: evaluating
/// Macaulay2 expressions the way the documentation functions do, and
/// recording what the documentation states.
struct DocumentationCollector<'source, S: SourceNavigation + ?Sized> {
    source: &'source S,
    /// Parses the expressions SimpleDoc strings embed.
    parser: SyntaxParser,
    documentation: PackageDocumentation,
    /// EXAMPLE applications evaluated inside SimpleDoc expressions.
    evaluated_examples: Vec<CodeFragment>,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read one call of a documentation `function` on `argument`.
    fn read_call(
        &mut self,
        call: SyntaxNode<'_>,
        function: DocumentationFunction,
        argument: SyntaxNode<'_>,
    ) {
        if function != DocumentationFunction::Undocumented {
            self.documentation
                .documentation_ranges
                .push(call.start_byte()..call.end_byte());
        }
        let argument = if argument.is_holder() {
            let Some(inner) = argument.collection_elements().next() else {
                return;
            };
            inner
        } else {
            argument
        };
        if matches!(
            function,
            DocumentationFunction::Doc | DocumentationFunction::Multidoc
        ) {
            if let Some(callee) = call.binary_parts().map(|parts| parts.left) {
                let range = self.source.range_for_node(callee);
                self.documentation.simple_doc_functions.push(range);
            }
        }
        match function {
            DocumentationFunction::Undocumented => {
                let keys = if argument.is_collection_expression() {
                    argument.collection_elements().collect()
                } else {
                    vec![argument]
                };
                for key in keys {
                    self.documentation.undocumented.push(DocumentationKey {
                        range: self.source.range_for_node(key),
                        subject: DocumentedSubject::of_expression(key),
                    });
                }
            }
            DocumentationFunction::Doc
            | DocumentationFunction::Multidoc
            | DocumentationFunction::Document
                if argument.is_string_literal() =>
            {
                // A plain string's escapes would shift every position, so
                // only one written without them maps back to the source.
                let Some(body) = argument.string_literal_inner_bytes() else {
                    return;
                };
                if argument.is::<m2_syn::nodes::StringLiteral>()
                    && self.source.text()[body.clone()].contains('\\')
                {
                    return;
                }
                self.read_simple_doc(call, body);
            }
            DocumentationFunction::Document if argument.is::<m2_syn::nodes::List>() => {
                self.read_hypertext_list(call, argument);
            }
            DocumentationFunction::Document if argument.is::<Sequence>() => {
                if argument
                    .collection_elements()
                    .any(|element| element.is_option_assignment())
                {
                    self.fault(
                        self.source.range_for_node(argument),
                        DocumentationFaultKind::OptionsOutsideList,
                    );
                }
            }
            DocumentationFunction::Doc
            | DocumentationFunction::Multidoc
            | DocumentationFunction::Document => {}
        }
    }

    /// Record `kind` of fault at `range`.
    fn fault(&mut self, range: TextRange, kind: DocumentationFaultKind) {
        self.documentation
            .faults
            .push(DocumentationFault { range, kind });
    }

    /// Record the symbols under `expression`, a node of the file's own syntax
    /// tree, as names the documentation evaluates.
    fn reference_code_names(&mut self, expression: SyntaxNode<'_>) {
        for symbol in expression.binding_references() {
            self.documentation.referenced_names.push(ReferencedName {
                span: self.source.span_for_node(symbol),
                site: NameSite::Code,
            });
        }
    }

    /// Evaluate the Macaulay2 expression at `bytes` of a SimpleDoc string,
    /// standing for `role`, as SimpleDoc does, inside parentheses, and hand
    /// its syntax tree to `inspect`. Its names are recorded as names the
    /// documentation evaluates and its tokens are marked; one that does not
    /// parse is a fault and is not inspected.
    fn evaluate_simple_doc_expression<R>(
        &mut self,
        bytes: ByteRange,
        role: EvaluatedExpression,
        inspect: impl FnOnce(SyntaxNode<'_>) -> R,
    ) -> Option<R> {
        let parenthesized = format!("({})", &self.source.text()[bytes.clone()]);
        let root = self.parser.parse(&parenthesized)?;
        let Some(expression) = Self::parsed_expression(root) else {
            let range = self.source.range_for_bytes(bytes);
            self.documentation.faults.push(DocumentationFault {
                range,
                kind: DocumentationFaultKind::MalformedExpression,
            });
            return None;
        };
        let origin = ParsedOrigin {
            source: bytes.start,
            added: 1,
        };
        for symbol in expression.binding_references() {
            let span = self.source.span_for_bytes(origin.source_bytes(symbol));
            self.documentation.referenced_names.push(ReferencedName {
                span,
                site: NameSite::SimpleDoc,
            });
        }
        let inner = if expression.is_holder() {
            expression
                .collection_elements()
                .next()
                .unwrap_or(expression)
        } else {
            expression
        };
        let key = (role == EvaluatedExpression::Key).then_some(inner);
        let code = CodeMarkup::of_tree(expression, key, origin);
        let inspected = inspect(inner);
        let tex_strings = HypertextMarkdown::tex_strings(expression)
            .into_iter()
            .map(|bytes| origin.source_range(bytes))
            .collect::<Vec<_>>();
        let hypertext = HypertextMarkdown::semantic_markup(expression);
        for mut markup in hypertext {
            markup.bytes = origin.source_range(markup.bytes);
            self.documentation.markup.push(markup);
        }
        for node in expression.descendants() {
            if let Some(argument) = HypertextMarkdown::example_argument(node) {
                self.documentation
                    .example_ranges
                    .push(origin.source_bytes(node));
                self.evaluated_examples.extend(Self::example_fragments(
                    argument,
                    self.source,
                    Some(origin),
                ));
            }
        }
        for bytes in tex_strings {
            self.render_tex(bytes);
        }
        self.record_code_markup(code);
        Some(inspected)
    }

    /// Mark the tokens of the code at `bytes` of a SimpleDoc string that
    /// SimpleDoc only shows, never evaluates — a `Usage` line, a line of a
    /// `CannedExample`. Code that does not parse stays prose.
    fn mark_shown_code(&mut self, bytes: ByteRange) {
        let source = self.source;
        let Some(root) = self.parser.parse(&source.text()[bytes.clone()]) else {
            return;
        };
        if root.has_error() {
            return;
        }
        let origin = ParsedOrigin {
            source: bytes.start,
            added: 0,
        };
        let code = CodeMarkup::of_tree(root, None, origin);
        self.record_code_markup(code);
    }

    /// Record `node`, a node of the file's own syntax tree that is a
    /// documentation key or a link to one: as naming its key when it is a
    /// string holding a symbol's name, and its option when it names an
    /// optional argument.
    fn record_key(&mut self, node: SyntaxNode<'_>) {
        let code = CodeMarkup {
            markup: DocumentedSubject::optional_argument_parts(node)
                .map(|(_, option)| DocumentationMarkup {
                    bytes: option.start_byte()..option.end_byte(),
                    kind: DocumentationMarkupKind::OptionName,
                })
                .into_iter()
                .collect(),
            key_string_candidates: node.string_literal_inner_bytes().into_iter().collect(),
        };
        self.record_code_markup(code);
    }

    /// Record the marks of `code`, once its syntax tree is gone: its strings
    /// in key positions name keys when they hold a symbol's name.
    fn record_code_markup(&mut self, code: CodeMarkup) {
        self.documentation.markup.extend(code.markup);
        let source = self.source;
        for bytes in code.key_string_candidates {
            if !self.names_symbol(&source.text()[bytes.clone()]) {
                continue;
            }
            self.documentation
                .key_strings
                .push(self.source.span_for_bytes(bytes.clone()));
            self.documentation.markup.push(DocumentationMarkup {
                bytes,
                kind: DocumentationMarkupKind::Name,
            });
        }
    }

    /// Whether `text` is the name of a symbol, as Macaulay2 parses it.
    fn names_symbol(&mut self, text: &str) -> bool {
        self.parser
            .parse(text)
            .and_then(Self::parsed_expression)
            .is_some_and(|expression| expression.is::<Symbol>() && expression.text() == text)
    }

    /// The one expression a separately parsed `root` holds, when it parses.
    fn parsed_expression(root: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
        root.named_children()
            .find(|cell| cell.is_source_cell())
            .and_then(|cell| cell.named_children().find(|child| !child.is_comment()))
            .filter(|_| !root.has_error())
    }
}

/// What a Macaulay2 expression SimpleDoc evaluates stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvaluatedExpression {
    /// A documentation key or a link to one — a `Key`, a `SeeAlso` entry, a
    /// menu entry — which a string names by its content.
    Key,
    /// A value, such as hypertext, a type, or an option name.
    Value,
}

/// Where the bytes of a syntax tree lie in the document. The expressions a
/// SimpleDoc string embeds are parsed on their own, from a copy of their
/// text that the parentheses SimpleDoc evaluates them in may extend.
#[derive(Debug, Clone, Copy)]
struct ParsedOrigin {
    /// The document byte the copied text starts at.
    source: usize,
    /// The bytes the copy adds before the text.
    added: usize,
}

impl ParsedOrigin {
    /// The document bytes of `node`.
    fn source_bytes(self, node: SyntaxNode<'_>) -> ByteRange {
        self.source_range(node.start_byte()..node.end_byte())
    }

    /// The document bytes of the parsed bytes `parsed`.
    fn source_range(self, parsed: ByteRange) -> ByteRange {
        self.source + parsed.start - self.added..self.source + parsed.end - self.added
    }
}

/// The marks of one piece of code a documentation string embeds, taken while
/// its syntax tree is alive. Whether a string in a key position names a key
/// is decided after, by parsing its content.
#[derive(Debug, Default)]
struct CodeMarkup {
    markup: Vec<DocumentationMarkup>,
    /// The document bytes of the content of each string in a key position:
    /// the key itself, or the key a `TO`, `TOH`, or `TO2` link links to.
    key_string_candidates: Vec<ByteRange>,
}

impl CodeMarkup {
    /// The marks of the tokens under `root`, placed at `origin`. `key` is the
    /// node of the tree that is a documentation key, if any.
    fn of_tree(root: SyntaxNode<'_>, key: Option<SyntaxNode<'_>>, origin: ParsedOrigin) -> Self {
        let link_keys = root
            .descendants()
            .filter_map(HypertextMarkdown::link_key)
            .chain(key)
            .collect::<Vec<_>>();
        let option_names = link_keys
            .iter()
            .filter_map(|key| DocumentedSubject::optional_argument_parts(*key))
            .map(|(_, option)| option.id())
            .collect::<Vec<_>>();
        let link_keys = link_keys.iter().map(|key| key.id()).collect::<Vec<_>>();
        let mut code = Self::default();
        for node in root.descendants() {
            let bytes = origin.source_bytes(node);
            if bytes.is_empty() {
                continue;
            }
            if node.is::<Symbol>() {
                code.markup.push(DocumentationMarkup {
                    bytes,
                    kind: if option_names.contains(&node.id()) {
                        DocumentationMarkupKind::OptionName
                    } else {
                        HypertextMarkdown::constructor_markup(node.text())
                            .unwrap_or(DocumentationMarkupKind::Name)
                    },
                });
            } else if node.is_string_literal() {
                if link_keys.contains(&node.id()) {
                    code.key_string_candidates.extend(
                        node.string_literal_inner_bytes()
                            .map(|inner| origin.source_range(inner)),
                    );
                }
            } else if let Some(token_type) = syntax_semantic_token_type(node)
                .filter(|token_type| *token_type != M2SemanticTokenType::Comment)
            {
                code.markup.push(DocumentationMarkup {
                    bytes,
                    kind: DocumentationMarkupKind::Syntax(token_type),
                });
            }
        }
        code
    }
}
