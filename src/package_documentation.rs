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

use m2_syn::nodes::{Expr, ExprCollection, ExprOpBinAdj, PackedContent, Symbol};
use m2_syn::visit::{self, Visit};
use m2_syn::{Lex, Spanned, TokenTree};
use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::Range as TextRange;

use crate::analysis::Analysis;
use crate::documentation::{DocumentationBlock, DocumentationTarget};
use crate::macaulay2::Macaulay2Conventions;
use crate::meta::BindingRole;
use crate::node_metadata::SyntaxSnapshot;
use crate::object_registry::ObjectName;
use crate::semantic_token::{lexical_semantic_token_type, M2SemanticTokenType};
use crate::source::{ByteRange, DocumentSource, DocumentSpan, SourceNavigation, SymbolMention};
use crate::source_package::{CodeFragment, PackageSessionCode, SourcePackages};
use crate::string_literal::{LiteralSource, StringDelimiter};
use crate::syntax::{Applications, BindingReferences, ExpressionSyntax};

pub use hypertext::DocumentOption;
use hypertext::{HypertextMarkdown, SemanticMarkup, TexStrings};
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
    /// Documentation nodes parsed from source declarations.
    nodes: Vec<DocumentationNode>,
    /// Keys explicitly declared as lacking documentation.
    undocumented: Vec<DocumentationKey>,
    /// Object names mentioned by documentation links.
    referenced_names: Vec<ReferencedName>,
    /// Strings naming a documentation key by a symbol's name, such as `"f"`
    /// in `TO "f"`: names, but not ones the documentation evaluates.
    key_strings: Vec<DocumentSpan>,
    /// Source spans classified as documentation markup.
    markup: Vec<DocumentationMarkup>,
    /// Source intervals belonging to package documentation.
    documentation_ranges: Vec<ByteRange>,
    /// Source intervals treated as executable documentation examples.
    example_ranges: Vec<ByteRange>,
    /// Malformed or inconsistent documentation entries found during extraction.
    faults: Vec<DocumentationFault>,
    /// Executable examples paired with their package-loading context.
    example_sessions: Vec<PackageSessionCode>,
    /// Source extents of functions that contribute SimpleDoc documentation.
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
        /// Function expression owning this semantic context.
        function: ObjectName,
        /// Argument identities accepted by the method signature.
        domain: Vec<ObjectName>,
    },
    /// An optional argument of a function, written as the array `[f, Limit]`
    /// of the function and the option's symbol.
    OptionalArgument {
        /// Function expression owning this semantic context.
        function: ObjectName,
        /// Name of the callable option being checked.
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
    /// A quoted documentation name, with lookup restricted to the literal content.
    QuotedName {
        /// First byte of the symbol name inside the string.
        start: usize,
        /// First byte following the symbol name.
        end: usize,
    },
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
    /// The address of a web link, `\url{…}`, `HREF`.
    Url,
    /// TeX mathematics, `$…$`.
    Tex,
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
        /// Section or constructor keyword recognized in the documentation.
        keyword: String,
        /// Keywords or section kinds accepted at this location.
        expected: Vec<SimpleDocKeyword>,
    },
    /// An option of a hypertext list that is not a documentation option.
    UnknownDocumentOption {
        #[doc = "Name of the callable option being checked."]
        option: String,
    },
    /// A section given twice in one page.
    RepeatedSection {
        #[doc = "Section name associated with the documentation finding."]
        section: String,
    },
    /// A section missing the content it needs, described by `expectation`.
    EmptySection {
        /// Section name associated with the documentation finding.
        section: String,
        /// Explanation of the form required by this documentation section.
        expectation: &'static str,
    },
    /// A `Headline` or `Heading` continued on further lines.
    MultilineHeadline {
        #[doc = "Section name associated with the documentation finding."]
        section: String,
    },
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
    ExpectedList {
        #[doc = "Name of the callable option being checked."]
        option: String,
    },
    /// `Inputs` or `Outputs` without the `Usage` they describe.
    UsageRequired,
    /// A page without a `Key`.
    MissingKey,
    /// A headline longer than the 100 characters Macaulay2 allows; it rejects
    /// one longer than 200 outright.
    HeadlineTooLong {
        #[doc = "Headline length measured against the documentation length limit."]
        length: usize,
    },
}

impl PackageDocumentation {
    /// The documentation written in the syntax of `source`, a file that
    /// declares `packages`, as `analysis` characterized it.
    pub fn collect(
        syntax: &SyntaxSnapshot,
        source: &(impl SourceNavigation + ?Sized),
        packages: &SourcePackages,
    ) -> Self {
        let mut collector = DocumentationCollector {
            source,
            documentation: Self::default(),
            evaluated_examples: Vec::new(),
        };
        let mut applications = Applications::default();
        syntax.visit(&mut applications);
        for call in applications.0 {
            let Some(function) = call
                .left_operand
                .symbol_name()
                .and_then(|name| name.parse::<DocumentationFunction>().ok())
            else {
                continue;
            };
            collector.read_call(call, function, &call.right_operand);
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
    fn of_expression<N: SourceNavigation + ?Sized>(code: &Code<'_, N>, expression: &Expr) -> Self {
        if let Expr::Symbol(symbol) = expression {
            return Self::Name(ObjectName::new(symbol.text()));
        }
        if let Some(name) = code.content_text(expression) {
            return Self::Name(ObjectName::new(name));
        }
        if let Some((function, option)) = Self::optional_argument_parts(expression) {
            return Self::OptionalArgument {
                function: ObjectName::new(function.text()),
                option: ObjectName::new(option.text()),
            };
        }
        if let Expr::Collection(ExprCollection::Sequence(_)) = expression {
            let mut elements = expression.contained_expressions().into_iter();
            let function = elements.next().and_then(Self::symbol);
            let domain = elements.map(Self::symbol).collect::<Option<Vec<_>>>();
            if let (Some(function), Some(domain)) = (function, domain) {
                if !domain.is_empty() {
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
        Self::Expression(code.text(expression).split_whitespace().collect())
    }

    /// The symbol `expression` is, if it is exactly one.
    fn symbol(expression: &Expr) -> Option<&Symbol> {
        match expression {
            Expr::Symbol(symbol) => Some(symbol),
            _ => None,
        }
    }

    /// The function and the option of `expression` when it is a key of an
    /// optional argument, the array `[f, Limit]`.
    fn optional_argument_parts(expression: &Expr) -> Option<(&Symbol, &Symbol)> {
        let Expr::Collection(ExprCollection::Array(_)) = expression else {
            return None;
        };
        match expression.contained_expressions()[..] {
            [function, option] => Some((Self::symbol(function)?, Self::symbol(option)?)),
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
    /// Optional short summary introducing the documentation node.
    headline: Option<String>,
    /// Rendered call forms illustrating use of the documented object.
    usage: Vec<String>,
    /// Descriptions of the callable's input parameters.
    inputs: Vec<String>,
    /// The `Inputs` items naming an optional argument, `Opt => …`.
    options: Vec<String>,
    /// Descriptions of the callable's return values.
    outputs: Vec<String>,
    /// Documented effects beyond the callable's return value.
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
    /// Source data supplying text and coordinate conversions.
    source: &'source S,
    /// Documentation extracted from the current document or selected object.
    documentation: PackageDocumentation,
    /// EXAMPLE applications evaluated inside SimpleDoc expressions.
    evaluated_examples: Vec<CodeFragment>,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read one call of a documentation `function` on `argument`.
    fn read_call(&mut self, call: &ExprOpBinAdj, function: DocumentationFunction, argument: &Expr) {
        let source = self.source;
        let code = Code::in_document(source);
        if function != DocumentationFunction::Undocumented {
            self.documentation
                .documentation_ranges
                .push(code.bytes(call));
        }
        let argument = if matches!(argument, Expr::Holder(_)) {
            let Some(inner) = argument.contained_expressions().into_iter().next() else {
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
            let range = source.range_for_bytes(code.bytes(&*call.left_operand));
            self.documentation.simple_doc_functions.push(range);
        }
        match function {
            DocumentationFunction::Undocumented => {
                let keys = if argument.collection_elements().is_some() {
                    argument.contained_expressions()
                } else {
                    vec![argument]
                };
                for key in keys {
                    self.documentation.undocumented.push(DocumentationKey {
                        range: source.range_for_bytes(code.bytes(key)),
                        subject: DocumentedSubject::of_expression(&code, key),
                    });
                }
            }
            DocumentationFunction::Doc
            | DocumentationFunction::Multidoc
            | DocumentationFunction::Document
                if code.content_bytes(argument).is_some() =>
            {
                // A plain string's escapes would shift every position, so
                // only one written without them maps back to the source.
                let Some(body) = code.content_bytes(argument) else {
                    return;
                };
                if StringDelimiter::of_expression(argument) == Some(StringDelimiter::Quote)
                    && source.text()[body.clone()].contains('\\')
                {
                    return;
                }
                self.read_simple_doc(call, body);
            }
            DocumentationFunction::Document
                if matches!(argument, Expr::Collection(ExprCollection::List(_))) =>
            {
                self.read_hypertext_list(call, argument);
            }
            DocumentationFunction::Document
                if matches!(argument, Expr::Collection(ExprCollection::Sequence(_))) =>
            {
                if argument
                    .contained_expressions()
                    .into_iter()
                    .any(|element| matches!(element, Expr::Option(_)))
                {
                    self.fault(
                        source.range_for_bytes(code.bytes(argument)),
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

    /// Record the symbols under `expression`, written in `code`, as names the
    /// documentation evaluates.
    fn reference_code_names<N: SourceNavigation + ?Sized>(
        &mut self,
        code: &Code<'_, N>,
        expression: &Expr,
        site: NameSite,
    ) {
        let mut references = BindingReferences::default();
        references.visit_expr(expression);
        for symbol in references.0 {
            self.documentation.referenced_names.push(ReferencedName {
                span: self.source.span_for_bytes(code.bytes(symbol)),
                site,
            });
        }
    }

    /// Evaluate the Macaulay2 expression at `bytes` of a SimpleDoc string,
    /// standing for `role`, as SimpleDoc does, inside parentheses, and hand
    /// its syntax to `inspect`. Its names are recorded as names the
    /// documentation evaluates and its tokens are marked; one that does not
    /// parse is a fault and is not inspected.
    fn evaluate_simple_doc_expression<R>(
        &mut self,
        bytes: ByteRange,
        role: EvaluatedExpression,
        inspect: impl FnOnce(&Code<'_, DocumentSource>, &Expr) -> R,
    ) -> Option<R> {
        let parenthesized = format!("({})", &self.source.text()[bytes.clone()]);
        let embedded = EmbeddedCode::parse(parenthesized);
        let Some(expression) = embedded.expression() else {
            let range = self.source.range_for_bytes(bytes);
            self.documentation.faults.push(DocumentationFault {
                range,
                kind: DocumentationFaultKind::MalformedExpression,
            });
            return None;
        };
        let code = embedded.code(ParsedOrigin {
            source: bytes.start,
            added: 1,
        });
        self.reference_code_names(&code, expression, NameSite::SimpleDoc);
        let inner = match expression.contained_expressions()[..] {
            [inner] if matches!(expression, Expr::Holder(_)) => inner,
            _ => expression,
        };
        let key = (role == EvaluatedExpression::Key).then_some(inner);
        let marks = CodeMarkup::of_syntax(&embedded.syntax, key, &code);
        let inspected = inspect(&code, inner);
        let mut applications = Applications::default();
        embedded.syntax.visit(&mut applications);
        let mut tex_strings = TexStrings::new(&code);
        tex_strings.visit_expr(expression);
        let mut semantic = SemanticMarkup::new(&code);
        semantic.visit_expr(expression);
        self.documentation.markup.extend(semantic.markup);
        for call in applications.0 {
            if let Some(argument) = HypertextMarkdown::example_argument(call) {
                self.documentation.example_ranges.push(code.bytes(call));
                self.evaluated_examples.extend(Self::example_fragments(
                    argument,
                    &code,
                    self.source,
                ));
            }
        }
        for bytes in tex_strings.strings {
            self.render_tex(bytes);
        }
        self.record_code_markup(marks);
        Some(inspected)
    }

    /// Mark the tokens of the code at `bytes` of a SimpleDoc string that
    /// SimpleDoc only shows, never evaluates — a `Usage` line, a line of a
    /// `CannedExample`. Code that does not parse stays prose.
    fn mark_shown_code(&mut self, bytes: ByteRange) {
        let embedded = EmbeddedCode::parse(self.source.text()[bytes.clone()].to_string());
        if !embedded.parsed_cleanly() {
            return;
        }
        let code = embedded.code(ParsedOrigin {
            source: bytes.start,
            added: 0,
        });
        let marks = CodeMarkup::of_syntax(&embedded.syntax, None, &code);
        self.record_code_markup(marks);
    }

    /// Record `key`, written in `code`, that is a documentation key or a link
    /// to one: as naming its key when it is a string holding a symbol's name,
    /// and its option when it names an optional argument.
    fn record_key<N: SourceNavigation + ?Sized>(&mut self, code: &Code<'_, N>, key: &Expr) {
        let marks = CodeMarkup {
            markup: DocumentedSubject::optional_argument_parts(key)
                .map(|(_, option)| DocumentationMarkup {
                    bytes: code.bytes(option),
                    kind: DocumentationMarkupKind::OptionName,
                })
                .into_iter()
                .collect(),
            key_string_candidates: code
                .content_bytes(key)
                .map(|body| (body, code.bytes(key)))
                .into_iter()
                .collect(),
        };
        self.record_code_markup(marks);
    }

    /// Record the marks of `code`, once its syntax tree is gone: its strings
    /// in key positions name keys when they hold a symbol's name.
    fn record_code_markup(&mut self, code: CodeMarkup) {
        self.documentation.markup.extend(code.markup);
        let source = self.source;
        for (bytes, literal) in code.key_string_candidates {
            if !self.names_symbol(&source.text()[bytes.clone()]) {
                continue;
            }
            self.documentation
                .key_strings
                .push(self.source.span_for_bytes(bytes.clone()));
            self.documentation.markup.push(DocumentationMarkup {
                bytes: literal,
                kind: DocumentationMarkupKind::QuotedName {
                    start: bytes.start,
                    end: bytes.end,
                },
            });
        }
    }

    /// Whether `text` is the name of a symbol, as Macaulay2 parses it.
    fn names_symbol(&mut self, text: &str) -> bool {
        let syntax = crate::node_metadata::SyntaxSnapshot::parse(text, None);
        let [cell] = syntax.cells() else {
            return false;
        };
        if cell.parse_error().is_some() || !cell.lexer_diagnostics().is_empty() {
            return false;
        }
        matches!(cell.syntax().map(|cell| &cell.content),
            Some(m2_syn::nodes::PackedContent::Expr(expression))
                if matches!(expression.as_ref(), m2_syn::nodes::Expr::Symbol(symbol) if symbol.text() == text))
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
    /// The document bytes of the parsed bytes `parsed`.
    fn source_range(self, parsed: ByteRange) -> ByteRange {
        self.source + parsed.start - self.added..self.source + parsed.end - self.added
    }
}

/// A separately parsed fragment of code a documentation string embeds, with
/// the coordinates of its own text.
struct EmbeddedCode {
    /// Syntax of the fragment.
    syntax: SyntaxSnapshot,
    /// The fragment's text and the provenance of its syntax.
    source: DocumentSource,
}

impl EmbeddedCode {
    /// Parses `text` on its own.
    fn parse(text: String) -> Self {
        let syntax = SyntaxSnapshot::parse(&text, None);
        let mut source = DocumentSource::new(text);
        source.register_syntax(&syntax);
        Self { syntax, source }
    }

    /// Whether the fragment lexed and parsed without error.
    fn parsed_cleanly(&self) -> bool {
        self.syntax.cells().iter().all(|cell| {
            cell.parse_error().is_none()
                && cell.lexer_diagnostics().is_empty()
                && cell.syntax().is_some()
        })
    }

    /// The one expression the fragment holds, when it parses.
    fn expression(&self) -> Option<&Expr> {
        let [cell] = self.syntax.cells() else {
            return None;
        };
        if !self.parsed_cleanly() {
            return None;
        }
        match &cell.syntax()?.content {
            PackedContent::Expr(expression) => Some(expression),
            PackedContent::Pack(_) => None,
        }
    }

    /// The fragment's syntax with its place in the document.
    fn code(&self, origin: ParsedOrigin) -> Code<'_, DocumentSource> {
        Code {
            navigation: &self.source,
            origin,
        }
    }
}

/// Syntax together with the coordinates it was parsed in and where those
/// lie in the document.
pub(super) struct Code<'text, Navigation: ?Sized> {
    /// Coordinates and text the syntax was registered with.
    navigation: &'text Navigation,
    /// Where the navigation's bytes lie in the document.
    origin: ParsedOrigin,
}

impl<'text, Navigation: SourceNavigation + ?Sized> Code<'text, Navigation> {
    /// Syntax written in the document itself.
    fn in_document(navigation: &'text Navigation) -> Self {
        Self {
            navigation,
            origin: ParsedOrigin {
                source: 0,
                added: 0,
            },
        }
    }

    /// The text of `syntax`.
    fn text(&self, syntax: &(impl Spanned + ?Sized)) -> &'text str {
        self.navigation
            .text_for_syntax(syntax)
            .expect("syntax is registered with the text it was parsed from")
    }

    /// The document bytes of `syntax`.
    fn bytes(&self, syntax: &(impl Spanned + ?Sized)) -> ByteRange {
        self.origin.source_range(
            self.navigation
                .syntax_bytes(syntax)
                .expect("syntax is registered with the text it was parsed from"),
        )
    }

    /// The document bytes of the content of the string literal `expression`.
    fn content_bytes(&self, expression: &Expr) -> Option<ByteRange> {
        expression
            .literal_content_bytes(self.navigation)
            .map(|bytes| self.origin.source_range(bytes))
    }

    /// The content of the string literal `expression`, as written.
    fn content_text(&self, expression: &Expr) -> Option<&'text str> {
        expression.literal_source(self.navigation)
    }
}

/// The marks of one piece of code a documentation string embeds, taken while
/// its syntax is alive. Whether a string in a key position names a key is
/// decided after, by parsing its content.
#[derive(Debug, Default)]
struct CodeMarkup {
    /// Source spans classified as documentation markup.
    markup: Vec<DocumentationMarkup>,
    /// The document bytes of the content of each string in a key position:
    /// the key itself, or the key a `TO`, `TOH`, or `TO2` link links to.
    key_string_candidates: Vec<(ByteRange, ByteRange)>,
}

impl CodeMarkup {
    /// The marks of the tokens of `syntax`, written in `code`. `key` is the
    /// expression of the syntax that is a documentation key, if any.
    fn of_syntax<N: SourceNavigation + ?Sized>(
        syntax: &SyntaxSnapshot,
        key: Option<&Expr>,
        code: &Code<'_, N>,
    ) -> Self {
        let mut applications = Applications::default();
        syntax.visit(&mut applications);
        let link_keys = applications
            .0
            .into_iter()
            .filter_map(HypertextMarkdown::link_key)
            .chain(key)
            .collect::<Vec<_>>();
        let option_names = link_keys
            .iter()
            .filter_map(|key| DocumentedSubject::optional_argument_parts(key))
            .map(|(_, option)| code.bytes(option))
            .collect::<Vec<_>>();
        let link_key_bytes = link_keys
            .iter()
            .map(|key| code.bytes(*key))
            .collect::<Vec<_>>();
        let mut marks = Self::default();
        let mut symbols = SymbolMarks {
            code,
            option_names: &option_names,
            link_key_bytes: &link_key_bytes,
            marks: &mut marks,
            symbol_starts: Vec::new(),
        };
        syntax.visit(&mut symbols);
        let symbol_starts = symbols.symbol_starts;
        for cell in syntax.cells().iter().filter_map(|cell| cell.syntax()) {
            marks.tokens(&cell.to_token_stream(), &symbol_starts, code);
        }
        marks
    }

    /// Marks the tokens of a stream that no symbol covers, descending into groups.
    fn tokens<N: SourceNavigation + ?Sized>(
        &mut self,
        stream: &m2_syn::TokenStream,
        symbol_starts: &[usize],
        code: &Code<'_, N>,
    ) {
        for token in stream {
            if let TokenTree::Group(group) = token {
                for span in std::iter::once(group.delimiter().opening_span())
                    .chain(group.delimiter().closing_span())
                {
                    let Some(parsed) = code.navigation.syntax_bytes(&span) else {
                        continue;
                    };
                    // Parentheses added to evaluate SimpleDoc are absent from the source.
                    if parsed.start < code.origin.added
                        || parsed.end > code.navigation.text().len() - code.origin.added
                    {
                        continue;
                    }
                    self.markup.push(DocumentationMarkup {
                        bytes: code.origin.source_range(parsed),
                        kind: DocumentationMarkupKind::Syntax(M2SemanticTokenType::Delimiter),
                    });
                }
                self.tokens(group.stream(), symbol_starts, code);
                continue;
            }
            let Some(token_type) = lexical_semantic_token_type(token)
                .or_else(|| {
                    matches!(token, TokenTree::Punct(_)).then_some(M2SemanticTokenType::Punctuation)
                })
                .filter(|token_type| {
                    !matches!(
                        token_type,
                        M2SemanticTokenType::Comment | M2SemanticTokenType::String
                    )
                })
            else {
                continue;
            };
            let bytes = code.bytes(token);
            if bytes.is_empty() || symbol_starts.contains(&bytes.start) {
                continue;
            }
            self.markup.push(DocumentationMarkup {
                bytes,
                kind: DocumentationMarkupKind::Syntax(token_type),
            });
        }
    }
}

/// Marks the symbols of code and finds the strings in key positions.
struct SymbolMarks<'a, 'text, N: ?Sized> {
    /// Coordinates of the visited syntax.
    code: &'a Code<'text, N>,
    /// The document bytes of symbols naming an optional argument.
    option_names: &'a [ByteRange],
    /// The document bytes of the documentation keys and link targets.
    link_key_bytes: &'a [ByteRange],
    /// Marks collected so far.
    marks: &'a mut CodeMarkup,
    /// The document bytes where symbols start.
    symbol_starts: Vec<usize>,
}

impl<'ast, N: SourceNavigation + ?Sized> Visit<'ast> for SymbolMarks<'_, '_, N> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.code.content_bytes(expression).is_some()
            && self.link_key_bytes.contains(&self.code.bytes(expression))
        {
            self.marks.key_string_candidates.extend(
                self.code
                    .content_bytes(expression)
                    .map(|body| (body, self.code.bytes(expression))),
            );
        }
        visit::visit_expr(self, expression);
    }

    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        let bytes = self.code.bytes(symbol);
        if bytes.is_empty() {
            return;
        }
        self.symbol_starts.push(bytes.start);
        self.marks.markup.push(DocumentationMarkup {
            kind: if self.option_names.contains(&bytes) {
                DocumentationMarkupKind::OptionName
            } else {
                HypertextMarkdown::constructor_markup(symbol.text())
                    .unwrap_or(DocumentationMarkupKind::Name)
            },
            bytes,
        });
    }
}
