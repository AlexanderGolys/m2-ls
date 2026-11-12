//! Documentation written as a hypertext list passed to `document`, the format
//! that predates SimpleDoc, and the hypertext both formats render.
//!
//! ```m2
//! document {
//!     Key => {f, (f, ZZ)},
//!     Headline => "a function",
//!     Usage => "f n",
//!     Inputs => {"n" => ZZ},
//!     "Some ", TT "text", " linking to ", TO g, ".",
//!     EXAMPLE {"f 3"},
//!     SeeAlso => {g}
//!     }
//! ```
//!
//! The list's options set the page's sections and must be among the options
//! `document` knows ([`DocumentOption`]), each given once; every other element
//! is the description. Hypertext is built from strings and the markup types of
//! the `Text` package — `PARA`, `TT`, `TO`, `UL`, … — which [`HypertextMarkdown`]
//! renders as Markdown. `@…@` expressions in SimpleDoc text evaluate to the
//! same hypertext.

use m2_syn::nodes::{Expr, ExprCollection, ExprOp, ExprOpBin, ExprOpBinAdj};
use m2_syn::visit::{self, Visit};
use strum::{Display, EnumIter, EnumString, IntoStaticStr};

use super::{
    Code, DocumentationCollector, DocumentationFaultKind, DocumentationKey, DocumentationMarkup,
    DocumentationMarkupKind, DocumentationNode, DocumentedSubject, NameSite, NodeMarkdown,
    TextStyle,
};
use crate::macaulay2::Macaulay2Conventions;
use crate::source::{ByteRange, SourceNavigation};
use crate::source_package::CodeFragment;
use crate::string_literal::StringDelimiter;
use crate::syntax::{Applications, ExpressionSyntax};

/// The options `document` accepts in its list (`documentOptions` in Core's
/// `document.m2`). The description has no option: it is every element of the
/// list that is not an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, Display, EnumIter)]
pub enum DocumentOption {
    /// The object or signature described by this documentation node.
    Key,
    /// Short summary introducing a documentation node.
    Headline,
    /// The base function to which this documentation belongs.
    BaseFunction,
    /// Example call forms for the documented callable.
    Usage,
    /// Descriptions of input arguments and accepted types.
    Inputs,
    /// Descriptions of return values and result types.
    Outputs,
    /// Effects beyond the callable's return value.
    Consequences,
    /// Acknowledgements associated with the documented work.
    Acknowledgement,
    /// People credited for the documented functionality.
    Contributors,
    /// Bibliographic or documentation references.
    References,
    /// Preferred citation for the documented work.
    Citation,
    /// Limitations or qualifications on the documented behavior.
    Caveat,
    /// Links to related documentation nodes.
    SeeAlso,
    /// Child nodes belonging beneath this documentation node.
    Subnodes,
    /// Source excerpts included in the rendered documentation.
    SourceCode,
    /// External source examples associated with this documentation.
    ExampleFiles,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;
    use strum::IntoEnumIterator;

    #[test]
    fn all_hypertext_constructors_are_marked_in_both_documentation_formats() {
        let builtins = ObjectRegistry::default();
        for constructor in MarkupFunction::iter() {
            let name: &str = constructor.into();
            let kind = match constructor.rendering() {
                MarkupRendering::Link => DocumentationMarkupKind::LinkConstructor,
                _ => DocumentationMarkupKind::TextMarkup,
            };
            for text in [
                format!("document {{Key => \"page\", {name} \"text\"}}\n"),
                format!("doc ///\nKey\n  page\nDescription\n  Text\n    @{name} \"text\"@\n///\n"),
            ] {
                let document = DocumentSnapshot::from_text(text.clone(), &builtins).unwrap();
                assert!(
                    document
                        .package_documentation()
                        .markup()
                        .iter()
                        .any(|markup| {
                            markup.kind == kind && &text[markup.bytes.clone()] == name
                        }),
                    "{name}: {text}"
                );
            }
        }
    }
}

/// The hypertext constructors of the `Text` package and Core that the
/// Markdown rendering distinguishes. Any other function applied in hypertext
/// renders as its Macaulay2 source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, EnumIter)]
#[allow(clippy::upper_case_acronyms)]
enum MarkupFunction {
    /// Link to a documentation node.
    TO,
    /// Link to a documentation node including its headline.
    TOH,
    /// Link with an explicit label and documentation target.
    TO2,
    /// Inline monospaced text.
    TT,
    /// Inline source code.
    CODE,
    /// Text representing keyboard input.
    KBD,
    /// Text representing program output.
    SAMP,
    /// Text representing a variable name.
    VAR,
    /// Macaulay2 source code with syntax-aware presentation.
    M2CODE,
    /// Bold text.
    BOLD,
    /// Strongly emphasized text.
    STRONG,
    /// Emphasized text.
    EM,
    /// Italic text.
    ITALIC,
    /// Hyperlink with an explicit external destination.
    HREF,
    /// A paragraph of documentation prose.
    PARA,
    /// A block of documentation content.
    DIV,
    /// A quoted block of documentation.
    BLOCKQUOTE,
    /// An indented documentation block.
    INDENT,
    /// Inline documentation content.
    SPAN,
    /// Text rendered at a smaller size.
    SMALL,
    /// Subscript text.
    SUB,
    /// Superscript text.
    SUP,
    /// A label for related documentation content.
    LABEL,
    /// Mathematical content expressed as TeX.
    TEX,
    /// Text preserved without interpreting its markup.
    LITERAL,
    /// An unordered list.
    UL,
    /// An ordered list.
    OL,
    /// A list of documentation navigation entries.
    MENU,
    /// A definition list.
    DL,
    /// A list item.
    LI,
    /// A definition term.
    DT,
    /// A definition description.
    DD,
    /// A subsection heading.
    SUBSECTION,
    /// A first-level heading.
    HEADER1,
    /// A second-level heading.
    HEADER2,
    /// A third-level heading.
    HEADER3,
    /// A fourth-level heading.
    HEADER4,
    /// A fifth-level heading.
    HEADER5,
    /// A sixth-level heading.
    HEADER6,
    /// A preformatted text block.
    PRE,
    /// An executable documentation example.
    EXAMPLE,
    /// A line break.
    BR,
    /// A horizontal separator.
    HR,
    /// Markup comment content.
    COMMENT,
    /// An image with its source and alternate text.
    IMG,
    /// A named hyperlink destination.
    ANCHOR,
    /// Document body content.
    BODY,
    /// An interactive button element.
    BUTTON,
    /// Character data without markup interpretation.
    CDATA,
    /// Document header metadata.
    HEAD,
    /// A complete HTML document container.
    HTML,
    /// An embedded document frame.
    IFRAME,
    /// A form input element.
    INPUT,
    /// Content deferred until a later rendering pass.
    LATER,
    /// An external resource link.
    LINK,
    /// Document metadata.
    META,
    /// Script content retained as markup.
    SCRIPT,
    /// Style content retained as markup.
    STYLE,
    /// A table of documentation content.
    TABLE,
    /// A table data cell.
    TD,
    /// A table header cell.
    TH,
    /// The document title.
    TITLE,
    /// A table row.
    TR,
    /// An individual executable example inside a documentation constructor.
    ExampleItem,
}

/// How a [`MarkupFunction`] renders.
enum MarkupRendering {
    /// A link to a documentation key: `TO`, `TOH`, and `TO2`, whose second
    /// argument is the link text.
    Link,
    /// Inline code.
    Code,
    /// Bold text.
    Bold,
    /// Italic text.
    Italic,
    /// A hyperlink, `HREF url` or `HREF(url, text)`.
    Hyperlink,
    /// A paragraph of its own.
    Block,
    /// Its content, inline.
    Inline,
    /// A list of its elements.
    List,
    /// A heading.
    Heading,
    /// Preformatted text.
    Preformatted,
    /// Example code, one line per string.
    Example,
    /// A line break.
    LineBreak,
    /// A horizontal rule.
    Rule,
    /// Nothing: comments and images.
    Hidden,
}

impl MarkupFunction {
    /// Text style and block behavior used for this hypertext constructor.
    fn rendering(self) -> MarkupRendering {
        use MarkupFunction::*;
        match self {
            TO | TOH | TO2 => MarkupRendering::Link,
            TT | CODE | KBD | SAMP | VAR | M2CODE => MarkupRendering::Code,
            BOLD | STRONG => MarkupRendering::Bold,
            EM | ITALIC => MarkupRendering::Italic,
            HREF => MarkupRendering::Hyperlink,
            PARA | DIV | BLOCKQUOTE | INDENT => MarkupRendering::Block,
            SPAN | SMALL | SUB | SUP | LABEL | TEX | LITERAL | LI | DT | DD | ANCHOR | BODY
            | BUTTON | CDATA | HEAD | HTML | IFRAME | INPUT | LATER | LINK | META | SCRIPT
            | STYLE | TABLE | TD | TH | TITLE | TR => MarkupRendering::Inline,
            UL | OL | MENU | DL => MarkupRendering::List,
            SUBSECTION | HEADER1 | HEADER2 | HEADER3 | HEADER4 | HEADER5 | HEADER6 => {
                MarkupRendering::Heading
            }
            PRE => MarkupRendering::Preformatted,
            EXAMPLE | ExampleItem => MarkupRendering::Example,
            BR => MarkupRendering::LineBreak,
            HR => MarkupRendering::Rule,
            COMMENT | IMG => MarkupRendering::Hidden,
        }
    }
}

/// Renders hypertext — strings and markup applications — as Markdown.
pub(super) struct HypertextMarkdown;

impl HypertextMarkdown {
    /// The markup a symbol is when it names a Text or Core hypertext
    /// constructor: a link constructor, or markup around text.
    pub(super) fn constructor_markup(name: &str) -> Option<DocumentationMarkupKind> {
        let function = name.parse::<MarkupFunction>().ok()?;
        Some(match function.rendering() {
            MarkupRendering::Link => DocumentationMarkupKind::LinkConstructor,
            _ => DocumentationMarkupKind::TextMarkup,
        })
    }

    /// Code argument of a recognized example constructor, when present.
    pub(super) fn example_argument(call: &ExprOpBinAdj) -> Option<&Expr> {
        let (function, argument) = markup_call(call)?;
        matches!(function.rendering(), MarkupRendering::Example).then_some(argument)
    }

    /// The key `call` links to, when it is a `TO`, `TOH`, or `TO2` link.
    pub(super) fn link_key(call: &ExprOpBinAdj) -> Option<&Expr> {
        Self::link_parts(call)?.first().copied()
    }

    /// The parts of a `TO`, `TOH`, or `TO2` link: the key it links to and, for
    /// `TO2`, the text it shows instead. `TO2` takes them as a sequence,
    /// `TO2(key, "text")`, or as a list, `TO2 {key, "text"}`.
    fn link_parts(call: &ExprOpBinAdj) -> Option<Vec<&Expr>> {
        let (function, argument) = markup_call(call)?;
        if !matches!(function.rendering(), MarkupRendering::Link) {
            return None;
        }
        Some(match Self::arguments(argument)[..] {
            [list @ Expr::Collection(ExprCollection::List(_))]
                if function == MarkupFunction::TO2 =>
            {
                list.contained_expressions()
            }
            ref parts => parts.to_vec(),
        })
    }

    /// The text a `TO2` link shows in place of its key's name.
    fn link_alias(expression: &Expr) -> Option<&Expr> {
        let Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) = expression else {
            return None;
        };
        Self::link_text(call)
    }

    /// The text `call` shows in place of its key's name, when it is a `TO2` link.
    pub(super) fn link_text(call: &ExprOpBinAdj) -> Option<&Expr> {
        let (function, _) = markup_call(call)?;
        (function == MarkupFunction::TO2)
            .then(|| Self::link_parts(call)?.get(1).copied())
            .flatten()
    }

    /// The arguments of a markup application: the elements of a sequence or
    /// parenthesized argument, or the argument itself, a list among them.
    /// Options, such as `"class" => "x"`, set attributes and are skipped.
    fn arguments(argument: &Expr) -> Vec<&Expr> {
        if matches!(
            argument,
            Expr::Collection(ExprCollection::Sequence(_)) | Expr::Holder(_)
        ) {
            argument
                .contained_expressions()
                .into_iter()
                .filter(|element| !matches!(element, Expr::Option(_)))
                .collect()
        } else {
            vec![argument]
        }
    }

    /// The elements of a collection argument, or the argument alone.
    fn list_elements(argument: &Expr) -> Vec<&Expr> {
        if argument.collection_elements().is_some() {
            argument.contained_expressions()
        } else {
            vec![argument]
        }
    }

    /// The elements of a list, or the expression alone.
    fn list_elements_if_list(value: &Expr) -> Vec<&Expr> {
        if matches!(value, Expr::Collection(ExprCollection::List(_))) {
            value.contained_expressions()
        } else {
            vec![value]
        }
    }
}

/// The markup function `call` applies, and its argument.
fn markup_call(call: &ExprOpBinAdj) -> Option<(MarkupFunction, &Expr)> {
    let function = call.left_operand.symbol_name()?.parse().ok()?;
    Some((function, &call.right_operand))
}

/// The markup function `expression` applies, and its argument.
fn markup_application(expression: &Expr) -> Option<(MarkupFunction, &Expr)> {
    match expression {
        Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => markup_call(call),
        _ => None,
    }
}

/// The style a markup function gives the strings it encloses, if any.
fn text_style(function: MarkupFunction) -> Option<TextStyle> {
    match function.rendering() {
        MarkupRendering::Code | MarkupRendering::Preformatted => Some(TextStyle::Monospace),
        MarkupRendering::Bold | MarkupRendering::Heading => Some(TextStyle::Strong),
        MarkupRendering::Italic => Some(TextStyle::Emphasis),
        MarkupRendering::Hyperlink => Some(TextStyle::Url),
        _ => None,
    }
}

/// Collects constructor names and styled string values.
pub(super) struct SemanticMarkup<'a, 'text, N: ?Sized> {
    /// Coordinates of the visited syntax.
    code: &'a Code<'text, N>,
    /// The markup found so far, in document bytes.
    pub(super) markup: Vec<DocumentationMarkup>,
    /// The markup functions enclosing the visited expression, outermost first.
    enclosing: Vec<MarkupFunction>,
}

impl<'a, 'text, N: ?Sized> SemanticMarkup<'a, 'text, N> {
    /// A collector for syntax written in `code`.
    pub(super) fn new(code: &'a Code<'text, N>) -> Self {
        Self {
            code,
            markup: Vec::new(),
            enclosing: Vec::new(),
        }
    }
}

impl<'ast, N: SourceNavigation + ?Sized> Visit<'ast> for SemanticMarkup<'_, '_, N> {
    fn visit_symbol(&mut self, symbol: &'ast m2_syn::nodes::Symbol) {
        if let Some(kind) = HypertextMarkdown::constructor_markup(symbol.text()) {
            self.markup.push(DocumentationMarkup {
                bytes: self.code.bytes(symbol),
                kind,
            });
        }
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.code.content_bytes(expression).is_some() {
            if let Some(style) = self.enclosing.iter().rev().find_map(|f| text_style(*f)) {
                self.markup.push(DocumentationMarkup {
                    bytes: self.code.bytes(expression),
                    kind: DocumentationMarkupKind::Styled(style),
                });
            }
        }
        if let Some(alias) = HypertextMarkdown::link_alias(expression) {
            if self.code.content_bytes(alias).is_some() {
                self.markup.push(DocumentationMarkup {
                    bytes: self.code.bytes(alias),
                    kind: DocumentationMarkupKind::Styled(TextStyle::Url),
                });
            }
        }
        let function = markup_application(expression).map(|(function, _)| function);
        self.enclosing.extend(function);
        visit::visit_expr(self, expression);
        if function.is_some() {
            self.enclosing.pop();
        }
    }
}

/// Collects string bodies interpreted as TeX prose, excluding code and
/// literal values.
pub(super) struct TexStrings<'a, 'text, N: ?Sized> {
    /// Coordinates of the visited syntax.
    code: &'a Code<'text, N>,
    /// The document bytes of the prose strings found so far.
    pub(super) strings: Vec<ByteRange>,
    /// How many enclosing expressions make their strings code or values.
    excluding: usize,
}

impl<'a, 'text, N: ?Sized> TexStrings<'a, 'text, N> {
    /// A collector for syntax written in `code`.
    pub(super) fn new(code: &'a Code<'text, N>) -> Self {
        Self {
            code,
            strings: Vec::new(),
            excluding: 0,
        }
    }
}

impl<N: SourceNavigation + ?Sized> TexStrings<'_, '_, N> {
    /// Whether the strings under `expression` are code or literal values.
    fn excludes(&self, expression: &Expr) -> bool {
        if let Some((function, _)) = markup_application(expression) {
            return matches!(
                function.rendering(),
                MarkupRendering::Code
                    | MarkupRendering::Preformatted
                    | MarkupRendering::Example
                    | MarkupRendering::Link
                    | MarkupRendering::Hyperlink
                    | MarkupRendering::Hidden
            ) || function == MarkupFunction::LITERAL;
        }
        match expression {
            Expr::Option(option) => matches!(
                self.code.text(&*option.key).parse::<DocumentOption>(),
                Ok(DocumentOption::Key
                    | DocumentOption::Usage
                    | DocumentOption::BaseFunction
                    | DocumentOption::SourceCode
                    | DocumentOption::ExampleFiles
                    | DocumentOption::SeeAlso)
            ),
            _ => false,
        }
    }
}

impl<'ast, N: SourceNavigation + ?Sized> Visit<'ast> for TexStrings<'_, '_, N> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.excluding == 0 {
            self.strings.extend(self.code.content_bytes(expression));
        }
        let excludes = self.excludes(expression);
        self.excluding += usize::from(excludes);
        visit::visit_expr(self, expression);
        self.excluding -= usize::from(excludes);
    }
}

impl<N: SourceNavigation + ?Sized> Code<'_, N> {
    /// The hypertext `expression` builds, as Markdown.
    pub(super) fn render(&self, expression: &Expr) -> String {
        if self.content_bytes(expression).is_some() {
            return self.string_value(expression);
        }
        if matches!(expression, Expr::Holder(_)) || expression.collection_elements().is_some() {
            return expression
                .contained_expressions()
                .into_iter()
                .filter(|element| !matches!(element, Expr::Option(_)))
                .map(|element| self.render(element))
                .collect();
        }
        let Some((function, argument)) = markup_application(expression) else {
            return format!("`{}`", self.text(expression));
        };
        let arguments = HypertextMarkdown::arguments(argument);
        let content = || {
            arguments
                .iter()
                .map(|argument| self.render(argument))
                .collect::<String>()
        };
        match function.rendering() {
            MarkupRendering::Link => match (function, arguments.as_slice()) {
                (MarkupFunction::TO2, [_, text, ..]) => self.render(text),
                _ => self.link_text(argument),
            },
            MarkupRendering::Code => format!("`{}`", content()),
            MarkupRendering::Bold => format!("**{}**", content()),
            MarkupRendering::Italic => format!("*{}*", content()),
            MarkupRendering::Hyperlink => match arguments.as_slice() {
                [url, text, ..] => format!("[{}]({})", self.render(text), self.render(url)),
                [url] => format!("<{}>", self.render(url)),
                [] => String::new(),
            },
            MarkupRendering::Block => format!("\n\n{}\n\n", content().trim()),
            MarkupRendering::Inline => content(),
            MarkupRendering::List => {
                let items = arguments
                    .iter()
                    .flat_map(|argument| HypertextMarkdown::list_elements(argument))
                    .map(|item| format!("- {}", self.render(item).trim()))
                    .collect::<Vec<_>>();
                format!("\n\n{}\n\n", items.join("\n"))
            }
            MarkupRendering::Heading => format!("\n\n#### {}\n\n", content().trim()),
            MarkupRendering::Preformatted => {
                format!(
                    "\n\n{}\n\n",
                    NodeMarkdown::fenced_code("", content().trim_end())
                )
            }
            MarkupRendering::Example => {
                let lines = arguments
                    .iter()
                    .flat_map(|argument| HypertextMarkdown::list_elements(argument))
                    .map(|line| self.render(line))
                    .collect::<Vec<_>>();
                format!(
                    "\n\n{}\n\n",
                    NodeMarkdown::fenced_code(
                        Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE,
                        lines.join("\n").trim_end()
                    )
                )
            }
            MarkupRendering::LineBreak => "  \n".to_string(),
            MarkupRendering::Rule => "\n\n---\n\n".to_string(),
            MarkupRendering::Hidden => String::new(),
        }
    }

    /// The text of a link to the documentation key `key`, or of a link
    /// `key` builds itself, such as `TO2(f, "text")`.
    pub(super) fn link_text(&self, key: &Expr) -> String {
        if markup_application(key).is_some() {
            return self.render(key);
        }
        if let (Expr::Holder(_), Some(inner)) =
            (key, key.contained_expressions().into_iter().next())
        {
            return self.link_text(inner);
        }
        let text = if self.content_bytes(key).is_some() {
            self.string_value(key)
        } else {
            self.text(key)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!("`{text}`")
    }

    /// The value of the string literal `literal`: a raw string as written, a
    /// plain string with its escapes read.
    pub(super) fn string_value(&self, literal: &Expr) -> String {
        let text = self.content_text(literal).unwrap_or_default();
        if StringDelimiter::of_expression(literal) != Some(StringDelimiter::Quote) {
            return text.to_string();
        }
        let mut value = String::with_capacity(text.len());
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character != '\\' {
                value.push(character);
                continue;
            }
            match characters.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some(escaped) => value.push(escaped),
                None => {}
            }
        }
        value
    }
}

/// What one hypertext list collects while its elements are read.
#[derive(Default)]
struct HypertextPage {
    /// Documentation keys identifying the objects described by this node.
    keys: Vec<DocumentationKey>,
    /// Rendered documentation or its extracted Markdown coordinates.
    markdown: NodeMarkdown,
    /// Code examples extracted from the documentation node.
    examples: Vec<CodeFragment>,
    /// Options or sections already encountered while parsing this documentation.
    seen: Vec<DocumentOption>,
    /// Human-readable description accumulated for this node.
    description: String,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read the hypertext `list` passed to the `document` `call`.
    pub(super) fn read_hypertext_list(&mut self, call: &ExprOpBinAdj, list: &Expr) {
        let source = self.source;
        let code = Code::in_document(source);
        let mut page = HypertextPage::default();
        let mut tex_strings = TexStrings::new(&code);
        tex_strings.visit_expr(list);
        for bytes in tex_strings.strings {
            self.render_tex(bytes);
        }
        let mut semantic = SemanticMarkup::new(&code);
        semantic.visit_expr(list);
        self.documentation.markup.extend(semantic.markup);
        for element in list.contained_expressions() {
            let Expr::Option(assignment) = element else {
                page.description.push_str(&code.render(element));
                continue;
            };
            let (option, value) = (&*assignment.key, &*assignment.value);
            let option_range = source.range_for_bytes(code.bytes(option));
            let known = match option {
                Expr::Symbol(symbol) => symbol.text().parse::<DocumentOption>().ok(),
                _ => None,
            };
            let Some(known) = known else {
                self.fault(
                    option_range,
                    DocumentationFaultKind::UnknownDocumentOption {
                        option: code.text(option).to_string(),
                    },
                );
                continue;
            };
            if page.seen.contains(&known) {
                self.fault(
                    option_range,
                    DocumentationFaultKind::RepeatedSection {
                        section: known.to_string(),
                    },
                );
            }
            page.seen.push(known);
            self.documentation.markup.push(DocumentationMarkup {
                bytes: code.bytes(option),
                kind: DocumentationMarkupKind::SectionKeyword,
            });
            self.read_document_option(&code, &mut page, known, value);
        }
        self.read_hypertext_links(&code, list, &mut page);

        if !page.seen.contains(&DocumentOption::Key) {
            self.fault(
                source.range_for_bytes(code.bytes(&*call.left_operand)),
                DocumentationFaultKind::MissingKey,
            );
        }
        let description = page.description.trim();
        if !description.is_empty() {
            page.markdown.description.insert(0, description.to_string());
        }
        self.documentation.nodes.push(DocumentationNode {
            range: source.range_for_bytes(code.bytes(call)),
            keys: page.keys,
            markdown: page.markdown.render(),
            examples: page.examples,
        });
    }

    /// Read the `value` of the documentation `option`.
    fn read_document_option(
        &mut self,
        code: &Code<'_, S>,
        page: &mut HypertextPage,
        option: DocumentOption,
        value: &Expr,
    ) {
        use DocumentOption::*;
        let source = self.source;
        let elements = || HypertextMarkdown::list_elements_if_list(value);
        match option {
            Key => {
                self.reference_code_names(code, value, NameSite::Code);
                for key in elements() {
                    self.record_key(code, key);
                    page.keys.push(DocumentationKey {
                        range: source.range_for_bytes(code.bytes(key)),
                        subject: DocumentedSubject::of_expression(code, key),
                    });
                }
            }
            Headline => {
                if let Some(bytes) = code.content_bytes(value) {
                    self.documentation.markup.push(DocumentationMarkup {
                        bytes,
                        kind: DocumentationMarkupKind::Styled(TextStyle::Strong),
                    });
                    let headline = code.string_value(value);
                    let length = headline.chars().count();
                    if length > Macaulay2Conventions::HEADLINE_WARNING_LENGTH {
                        self.fault(
                            source.range_for_bytes(code.bytes(value)),
                            DocumentationFaultKind::HeadlineTooLong { length },
                        );
                    }
                    page.markdown.headline = Some(headline);
                }
            }
            Usage => {
                if code.content_bytes(value).is_some() {
                    if let Some(bytes) = code.content_bytes(value).filter(|bytes| {
                        StringDelimiter::of_expression(value) != Some(StringDelimiter::Quote)
                            || !source.text()[bytes.clone()].contains('\\')
                    }) {
                        self.mark_shown_code(bytes);
                    }
                    page.markdown.usage.extend(
                        code.string_value(value)
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(str::to_string),
                    );
                }
            }
            Inputs | Outputs | Consequences => {
                if code.content_bytes(value).is_some() {
                    self.fault(
                        source.range_for_bytes(code.bytes(value)),
                        DocumentationFaultKind::ExpectedList {
                            option: option.to_string(),
                        },
                    );
                    return;
                }
                for item in elements() {
                    self.read_hypertext_item(code, item, option, &mut page.markdown);
                }
            }
            SeeAlso => {
                self.reference_code_names(code, value, NameSite::Code);
                for entry in elements() {
                    self.record_key(code, entry);
                }
                page.markdown
                    .see_also
                    .extend(elements().into_iter().map(|entry| code.link_text(entry)));
            }
            Subnodes => page.markdown.subnodes.extend(
                elements()
                    .into_iter()
                    .map(|entry| format!("- {}", code.render(entry).trim())),
            ),
            Acknowledgement | Contributors | References | Caveat => {
                let rendered = code.render(value);
                let rendered = rendered.trim();
                if !rendered.is_empty() {
                    page.markdown
                        .subsections
                        .push((option.to_string(), rendered.to_string()));
                }
            }
            BaseFunction | SourceCode => self.reference_code_names(code, value, NameSite::Code),
            Citation | ExampleFiles => {}
        }
    }

    /// Read one item of the `Inputs`, `Outputs`, or `Consequences` `option`
    /// into the parameters, options, returns, or consequences of `markdown`,
    /// as Markdown: `"n" => ZZ`, `"n" => {ZZ, "description"}`, an optional
    /// argument `Limit => …`, or plain hypertext.
    fn read_hypertext_item(
        &mut self,
        code: &Code<'_, S>,
        item: &Expr,
        option: DocumentOption,
        markdown: &mut NodeMarkdown,
    ) {
        let (rendered, names_option) = match item {
            Expr::Option(assignment) => {
                let (name, description) = (&*assignment.key, &*assignment.value);
                if matches!(option, DocumentOption::Inputs | DocumentOption::Outputs) {
                    if let Some(bytes) = code.content_bytes(name) {
                        self.documentation.markup.push(DocumentationMarkup {
                            bytes,
                            kind: DocumentationMarkupKind::ItemName,
                        });
                    }
                }
                self.reference_code_names(code, description, NameSite::Code);
                let description = HypertextMarkdown::list_elements_if_list(description)
                    .into_iter()
                    .map(|part| match part {
                        Expr::Symbol(symbol) => format!("`{}`", symbol.text()),
                        _ => code.render(part),
                    })
                    .collect::<Vec<_>>();
                let rendered_name = if code.content_bytes(name).is_some() {
                    code.string_value(name)
                } else {
                    code.link_text(name)
                };
                let (item_type, text) = description.split_first().map_or_else(
                    || (String::new(), String::new()),
                    |(item_type, text)| (item_type.clone(), text.join(" ")),
                );
                let item = NodeMarkdown::item(Some(&rendered_name), [item_type, text]);
                (item, matches!(name, Expr::Symbol(_)))
            }
            _ => (code.render(item).trim().to_string(), false),
        };
        match (option, names_option) {
            (DocumentOption::Inputs, true) => markdown.options.push(rendered),
            (DocumentOption::Inputs, false) => markdown.inputs.push(rendered),
            (DocumentOption::Outputs, _) => markdown.outputs.push(rendered),
            _ => markdown.consequences.push(rendered),
        }
    }

    /// Record the keys `TO`, `TOH`, and `TO2` link to anywhere in `list` as
    /// names the documentation evaluates, or names a string links by, and
    /// the code of its `EXAMPLE`s as the page's examples.
    fn read_hypertext_links(&mut self, code: &Code<'_, S>, list: &Expr, page: &mut HypertextPage) {
        let mut applications = Applications::default();
        applications.visit_expr(list);
        for call in applications.0 {
            if HypertextMarkdown::example_argument(call).is_some() {
                self.documentation.example_ranges.push(code.bytes(call));
            }
            if let Some(key) = HypertextMarkdown::link_key(call) {
                self.reference_code_names(code, key, NameSite::Code);
                self.record_key(code, key);
            } else if let Some(argument) = HypertextMarkdown::example_argument(call) {
                page.examples
                    .extend(Self::example_fragments(argument, code, self.source));
            }
        }
    }

    /// The code fragments of an `EXAMPLE` argument: a string, a list of
    /// strings, or `lines` of a string, each nonblank line of which is one
    /// input. A plain string's escapes would shift every position, so only
    /// one written without them maps back to the source.
    pub(super) fn example_fragments<N: SourceNavigation + ?Sized>(
        argument: &Expr,
        code: &Code<'_, N>,
        source: &S,
    ) -> Vec<CodeFragment> {
        let lines_argument = match argument {
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))
                if call.left_operand.symbol_name() == Some("lines") =>
            {
                Some(&*call.right_operand)
            }
            _ => None,
        };
        let strings = if matches!(argument, Expr::Collection(ExprCollection::List(_))) {
            argument.contained_expressions()
        } else {
            vec![lines_argument.unwrap_or(argument)]
        };
        let mut fragments = Vec::new();
        for string in strings {
            let Some(body) = code.content_bytes(string) else {
                continue;
            };
            let text = &source.text()[body.clone()];
            if StringDelimiter::of_expression(string) == Some(StringDelimiter::Quote)
                && text.contains('\\')
            {
                continue;
            }
            let mut offset = body.start;
            for line in text.split_inclusive('\n') {
                let content = line.trim_end();
                let start = offset + line.len() - line.trim_start().len();
                let end = offset + content.len();
                offset += line.len();
                if start < end {
                    fragments.push(CodeFragment::new(start..end, source));
                }
            }
        }
        fragments
    }
}
