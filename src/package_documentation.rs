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
//! - [`DocumentationMarkup`] spans that highlight the keywords and expressions
//!   SimpleDoc embeds in its string;
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

use m2_syn::nodes::{Sequence, Symbol};
use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::Range as TextRange;

use crate::analysis::Analysis;
use crate::documentation::{DocumentationBlock, DocumentationTarget};
use crate::meta::BindingRole;
use crate::node_metadata::{M2Node, M2Parser};
use crate::object_registry::ObjectName;
use crate::source::{ByteRange, DocumentSpan, SourceNavigation, SymbolMention};
use crate::source_package::{CodeFragment, PackageSessionCode, SourcePackages};

pub use hypertext::DocumentOption;
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
    markup: Vec<DocumentationMarkup>,
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
    /// Anything else, such as an overview page titled by a string with spaces
    /// (`"Using Graphs"`), an option (`[f, Limit]`), or an operator installation
    /// (`(symbol +, ZZ, ZZ)`), kept as its Macaulay2 source.
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

/// A span of a SimpleDoc string that is not prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationMarkup {
    /// The bytes of the span.
    pub bytes: ByteRange,
    /// What the span is.
    pub kind: DocumentationMarkupKind,
}

/// What a [`DocumentationMarkup`] span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentationMarkupKind {
    /// A section keyword, such as `Headline` or `Example`.
    SectionKeyword,
    /// A Macaulay2 expression the documentation evaluates in the package,
    /// such as a key, an `@TO f@` link, or a `SeeAlso` entry.
    Expression,
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
        root: M2Node<'_>,
        source: &(impl SourceNavigation + ?Sized),
        packages: &SourcePackages,
    ) -> Self {
        let mut collector = DocumentationCollector {
            source,
            parser: M2Parser::new().expect("the Macaulay2 grammar loads"),
            documentation: Self::default(),
        };
        for call in root.descendants() {
            let Some(function) = call
                .applied_symbol_name()
                .and_then(|name| name.parse::<DocumentationFunction>().ok())
            else {
                continue;
            };
            let Some(argument) = call.child_by_field_name("right") else {
                continue;
            };
            collector.read_call(call, function, argument);
        }
        let mut documentation = collector.documentation;
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

    /// The keyword and expression spans of SimpleDoc strings.
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

    /// The names SimpleDoc strings evaluate, which only the documentation
    /// knows to be names: navigation follows them as symbol mentions.
    pub fn mentions(&self) -> impl Iterator<Item = SymbolMention> + '_ {
        self.referenced_names
            .iter()
            .filter(|name| name.site == NameSite::SimpleDoc)
            .map(|name| SymbolMention::new(name.span.clone()))
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
    fn of_expression(expression: M2Node<'_>) -> Self {
        if expression.is::<Symbol>() {
            return Self::Name(ObjectName::new(expression.text()));
        }
        if let Some(name) = expression.string_literal_inner_text() {
            return Self::Name(ObjectName::new(name));
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
            Self::Expression(source) => source.clone(),
        })
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
            Self::Expression(_) => Vec::new(),
        }
    }
}

/// The content of one page, assembled section by section into the Markdown
/// hover shows: the headline, usage, inputs, outputs, consequences, the
/// description, the subsections such as `Caveat`, the menu of subnodes, and
/// the keys it links to.
#[derive(Debug, Default)]
struct NodeMarkdown {
    headline: Option<String>,
    usage: Vec<String>,
    inputs: Vec<String>,
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
            blocks.push(format!("*{headline}*"));
        }
        if !self.usage.is_empty() {
            blocks.push(Self::fenced_code("m2", &self.usage.join("\n")));
        }
        for (title, items) in [
            ("Inputs", &self.inputs),
            ("Outputs", &self.outputs),
            ("Consequences", &self.consequences),
        ] {
            if !items.is_empty() {
                blocks.push(format!("**{title}**\n\n{}", list(items)));
            }
        }
        blocks.extend(
            self.description
                .iter()
                .map(|block| block.trim().to_string())
                .filter(|block| !block.is_empty()),
        );
        for (title, content) in &self.subsections {
            blocks.push(format!("**{title}**\n\n{content}"));
        }
        if !self.subnodes.is_empty() {
            blocks.push(format!("**Menu**\n\n{}", self.subnodes.join("\n")));
        }
        if !self.see_also.is_empty() {
            blocks.push(format!("**See also:** {}", self.see_also.join(", ")));
        }
        blocks.join("\n\n")
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
    parser: M2Parser,
    documentation: PackageDocumentation,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read one call of a documentation `function` on `argument`.
    fn read_call(
        &mut self,
        call: M2Node<'_>,
        function: DocumentationFunction,
        argument: M2Node<'_>,
    ) {
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
            if let Some(callee) = call.child_by_field_name("left") {
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
    fn reference_code_names(&mut self, expression: M2Node<'_>) {
        for symbol in expression.binding_references() {
            self.documentation.referenced_names.push(ReferencedName {
                span: self.source.span_for_node(symbol),
                site: NameSite::Code,
            });
        }
    }

    /// Evaluate the Macaulay2 expression at `bytes` of a SimpleDoc string as
    /// SimpleDoc does, inside parentheses, and hand its syntax tree to
    /// `inspect`. Its names are recorded as names the documentation evaluates
    /// and its span as an expression; one that does not parse is a fault and
    /// is not inspected.
    fn evaluate_simple_doc_expression<R>(
        &mut self,
        bytes: ByteRange,
        inspect: impl FnOnce(M2Node<'_>) -> R,
    ) -> Option<R> {
        let parenthesized = format!("({})", &self.source.text()[bytes.clone()]);
        let root = self.parser.parse(&parenthesized)?;
        let expression = root
            .named_children()
            .find(|cell| cell.is_source_cell())
            .and_then(|cell| cell.named_children().find(|child| !child.is_comment()))
            .filter(|_| !root.has_error());
        let Some(expression) = expression else {
            let range = self.source.range_for_bytes(bytes);
            self.documentation.faults.push(DocumentationFault {
                range,
                kind: DocumentationFaultKind::MalformedExpression,
            });
            return None;
        };
        // The parentheses SimpleDoc adds shift every offset by one.
        let source_byte = |parsed: usize| bytes.start + parsed - 1;
        for symbol in expression.binding_references() {
            let span = self
                .source
                .span_for_bytes(source_byte(symbol.start_byte())..source_byte(symbol.end_byte()));
            self.documentation.referenced_names.push(ReferencedName {
                span,
                site: NameSite::SimpleDoc,
            });
        }
        self.documentation.markup.push(DocumentationMarkup {
            bytes,
            kind: DocumentationMarkupKind::Expression,
        });
        let inner = if expression.is_holder() {
            expression
                .collection_elements()
                .next()
                .unwrap_or(expression)
        } else {
            expression
        };
        Some(inspect(inner))
    }
}
