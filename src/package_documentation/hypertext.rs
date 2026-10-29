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

use m2_syn::nodes::{List, StringLiteral, Symbol};
use strum::{Display, EnumIter, EnumString, IntoStaticStr};

use super::{
    DocumentationCollector, DocumentationFaultKind, DocumentationKey, DocumentationMarkup,
    DocumentationMarkupKind, DocumentationNode, DocumentedSubject, NodeMarkdown, ParsedOrigin,
    TextStyle,
};
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::SyntaxNode;
use crate::source::SourceNavigation;
use crate::source_package::CodeFragment;

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
    pub(super) fn example_argument(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
        let (function, argument) = Self::markup_application(node)?;
        matches!(function.rendering(), MarkupRendering::Example).then_some(argument)
    }

    /// Constructor names and styled string values, in the syntax tree's coordinates.
    pub(super) fn semantic_markup(root: SyntaxNode<'_>) -> Vec<DocumentationMarkup> {
        let mut markup = Vec::new();
        for node in root.descendants() {
            if let Some(kind) = node
                .is::<Symbol>()
                .then(|| Self::constructor_markup(node.text()))
                .flatten()
            {
                markup.push(DocumentationMarkup {
                    bytes: node.start_byte()..node.end_byte(),
                    kind,
                });
            }
            let Some(bytes) = node.string_literal_inner_bytes() else {
                continue;
            };
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if let Some((function, _)) = Self::markup_application(parent) {
                    let style = match function.rendering() {
                        MarkupRendering::Code | MarkupRendering::Preformatted => {
                            Some(TextStyle::Monospace)
                        }
                        MarkupRendering::Bold | MarkupRendering::Heading => Some(TextStyle::Strong),
                        MarkupRendering::Italic => Some(TextStyle::Emphasis),
                        MarkupRendering::Hyperlink => Some(TextStyle::Link),
                        _ => None,
                    };
                    if let Some(style) = style {
                        markup.push(DocumentationMarkup {
                            bytes,
                            kind: DocumentationMarkupKind::Styled(style),
                        });
                        break;
                    }
                }
                if parent.id() == root.id() {
                    break;
                }
                ancestor = parent.parent();
            }
        }
        markup
    }

    /// String bodies interpreted as TeX prose, excluding code and literal values.
    pub(super) fn tex_strings(root: SyntaxNode<'_>) -> Vec<crate::source::ByteRange> {
        root.descendants()
            .filter_map(|node| {
                let bytes = node.string_literal_inner_bytes()?;
                let mut ancestor = node.parent();
                while let Some(parent) = ancestor {
                    if Self::markup_application(parent).is_some_and(|(function, _)| {
                        matches!(
                            function.rendering(),
                            MarkupRendering::Code
                                | MarkupRendering::Preformatted
                                | MarkupRendering::Example
                                | MarkupRendering::Link
                                | MarkupRendering::Hyperlink
                                | MarkupRendering::Hidden
                        ) || function == MarkupFunction::LITERAL
                    }) {
                        return None;
                    }
                    if parent.is_option_assignment()
                        && parent.binary_parts().is_some_and(|parts| {
                            matches!(
                                parts.left.text().parse::<DocumentOption>(),
                                Ok(DocumentOption::Key
                                    | DocumentOption::Usage
                                    | DocumentOption::BaseFunction
                                    | DocumentOption::SourceCode
                                    | DocumentOption::ExampleFiles
                                    | DocumentOption::SeeAlso)
                            )
                        })
                    {
                        return None;
                    }
                    if parent.id() == root.id() {
                        break;
                    }
                    ancestor = parent.parent();
                }
                Some(bytes)
            })
            .collect()
    }

    /// The hypertext `node` builds, as Markdown.
    pub(super) fn render(node: SyntaxNode<'_>) -> String {
        if node.is_string_literal() {
            return Self::string_value(node);
        }
        if node.is_collection_expression() || node.is_holder() {
            return node
                .collection_elements()
                .filter(|element| !element.is_option_assignment())
                .map(Self::render)
                .collect();
        }
        let Some((function, argument)) = Self::markup_application(node) else {
            return format!("`{}`", node.text());
        };
        let arguments = Self::arguments(argument);
        let content = || {
            arguments
                .iter()
                .copied()
                .map(Self::render)
                .collect::<String>()
        };
        match function.rendering() {
            MarkupRendering::Link => match (function, arguments.as_slice()) {
                (MarkupFunction::TO2, [_, text, ..]) => Self::render(*text),
                _ => Self::link_text(argument),
            },
            MarkupRendering::Code => format!("`{}`", content()),
            MarkupRendering::Bold => format!("**{}**", content()),
            MarkupRendering::Italic => format!("*{}*", content()),
            MarkupRendering::Hyperlink => match arguments.as_slice() {
                [url, text, ..] => format!("[{}]({})", Self::render(*text), Self::render(*url)),
                [url] => format!("<{}>", Self::render(*url)),
                [] => String::new(),
            },
            MarkupRendering::Block => format!("\n\n{}\n\n", content().trim()),
            MarkupRendering::Inline => content(),
            MarkupRendering::List => {
                let items = arguments
                    .iter()
                    .flat_map(|argument| {
                        if argument.is_collection_expression() {
                            argument.collection_elements().collect()
                        } else {
                            vec![*argument]
                        }
                    })
                    .map(|item| format!("- {}", Self::render(item).trim()))
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
                    .flat_map(|argument| {
                        if argument.is_collection_expression() {
                            argument.collection_elements().collect()
                        } else {
                            vec![*argument]
                        }
                    })
                    .map(Self::render)
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
    pub(super) fn link_text(key: SyntaxNode<'_>) -> String {
        if Self::markup_application(key).is_some() {
            return Self::render(key);
        }
        if key.is_holder() {
            if let Some(inner) = key.collection_elements().next() {
                return Self::link_text(inner);
            }
        }
        let text = if key.is_string_literal() {
            Self::string_value(key)
        } else {
            key.text().split_whitespace().collect::<Vec<_>>().join(" ")
        };
        format!("`{text}`")
    }

    /// The key `node` links to, when it is a `TO`, `TOH`, or `TO2` link.
    pub(super) fn link_key(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
        let (function, argument) = Self::markup_application(node)?;
        matches!(function.rendering(), MarkupRendering::Link)
            .then(|| Self::arguments(argument).first().copied())
            .flatten()
    }

    /// The markup function `node` applies, and its argument.
    fn markup_application(node: SyntaxNode<'_>) -> Option<(MarkupFunction, SyntaxNode<'_>)> {
        let function = node.applied_symbol_name()?.parse().ok()?;
        Some((function, node.binary_parts().map(|parts| parts.right)?))
    }

    /// The arguments of a markup application: the elements of a sequence or
    /// parenthesized argument, or the argument itself, a list among them.
    /// Options, such as `"class" => "x"`, set attributes and are skipped.
    fn arguments(argument: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
        if argument.is::<m2_syn::nodes::Sequence>() || argument.is_holder() {
            argument
                .collection_elements()
                .filter(|element| !element.is_option_assignment())
                .collect()
        } else {
            vec![argument]
        }
    }

    /// The value of the string literal `node`: a raw string as written, a
    /// plain string with its escapes read.
    fn string_value(node: SyntaxNode<'_>) -> String {
        let text = node.string_literal_inner_text().unwrap_or_default();
        if !node.is::<StringLiteral>() {
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
    pub(super) fn read_hypertext_list(&mut self, call: SyntaxNode<'_>, list: SyntaxNode<'_>) {
        let mut page = HypertextPage::default();
        for bytes in HypertextMarkdown::tex_strings(list) {
            self.render_tex(bytes);
        }
        self.documentation
            .markup
            .extend(HypertextMarkdown::semantic_markup(list));
        for element in list.collection_elements() {
            if !element.is_option_assignment() {
                page.description
                    .push_str(&HypertextMarkdown::render(element));
                continue;
            }
            let (Some(option), Some(value)) = (
                element.binary_parts().map(|parts| parts.left),
                element.binary_parts().map(|parts| parts.right),
            ) else {
                continue;
            };
            let option_range = self.source.range_for_node(option);
            let Some(known) = option
                .is::<Symbol>()
                .then(|| option.text().parse::<DocumentOption>().ok())
                .flatten()
            else {
                self.fault(
                    option_range,
                    DocumentationFaultKind::UnknownDocumentOption {
                        option: option.text().to_string(),
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
                bytes: option.start_byte()..option.end_byte(),
                kind: DocumentationMarkupKind::SectionKeyword,
            });
            self.read_document_option(&mut page, known, value);
        }
        self.read_hypertext_links(list, &mut page);

        if !page.seen.contains(&DocumentOption::Key) {
            let anchor = call.binary_parts().map(|parts| parts.left).unwrap_or(call);
            self.fault(
                self.source.range_for_node(anchor),
                DocumentationFaultKind::MissingKey,
            );
        }
        let description = page.description.trim();
        if !description.is_empty() {
            page.markdown.description.insert(0, description.to_string());
        }
        self.documentation.nodes.push(DocumentationNode {
            range: self.source.range_for_node(call),
            keys: page.keys,
            markdown: page.markdown.render(),
            examples: page.examples,
        });
    }

    /// Read the `value` of the documentation `option`.
    fn read_document_option(
        &mut self,
        page: &mut HypertextPage,
        option: DocumentOption,
        value: SyntaxNode<'_>,
    ) {
        use DocumentOption::*;
        let elements = || {
            if value.is::<List>() {
                value.collection_elements().collect()
            } else {
                vec![value]
            }
        };
        match option {
            Key => {
                self.reference_code_names(value);
                for key in elements() {
                    self.record_key(key);
                    page.keys.push(DocumentationKey {
                        range: self.source.range_for_node(key),
                        subject: DocumentedSubject::of_expression(key),
                    });
                }
            }
            Headline => {
                if value.is_string_literal() {
                    if let Some(bytes) = value.string_literal_inner_bytes() {
                        self.documentation.markup.push(DocumentationMarkup {
                            bytes,
                            kind: DocumentationMarkupKind::Styled(TextStyle::Strong),
                        });
                    }
                    let headline = HypertextMarkdown::string_value(value);
                    let length = headline.chars().count();
                    if length > Macaulay2Conventions::HEADLINE_WARNING_LENGTH {
                        self.fault(
                            self.source.range_for_node(value),
                            DocumentationFaultKind::HeadlineTooLong { length },
                        );
                    }
                    page.markdown.headline = Some(headline);
                }
            }
            Usage => {
                if value.is_string_literal() {
                    if let Some(bytes) = value.string_literal_inner_bytes().filter(|bytes| {
                        !value.is::<StringLiteral>()
                            || !self.source.text()[bytes.clone()].contains('\\')
                    }) {
                        self.mark_shown_code(bytes);
                    }
                    page.markdown.usage.extend(
                        HypertextMarkdown::string_value(value)
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(str::to_string),
                    );
                }
            }
            Inputs | Outputs | Consequences => {
                if value.is_string_literal() {
                    self.fault(
                        self.source.range_for_node(value),
                        DocumentationFaultKind::ExpectedList {
                            option: option.to_string(),
                        },
                    );
                    return;
                }
                for item in elements() {
                    self.read_hypertext_item(item, option, &mut page.markdown);
                }
            }
            SeeAlso => {
                self.reference_code_names(value);
                for entry in elements() {
                    self.record_key(entry);
                }
                page.markdown
                    .see_also
                    .extend(elements().into_iter().map(HypertextMarkdown::link_text));
            }
            Subnodes => page.markdown.subnodes.extend(
                elements()
                    .into_iter()
                    .map(|entry| format!("- {}", HypertextMarkdown::render(entry).trim())),
            ),
            Acknowledgement | Contributors | References | Caveat => {
                let rendered = HypertextMarkdown::render(value);
                let rendered = rendered.trim();
                if !rendered.is_empty() {
                    page.markdown
                        .subsections
                        .push((option.to_string(), rendered.to_string()));
                }
            }
            BaseFunction | SourceCode => self.reference_code_names(value),
            Citation | ExampleFiles => {}
        }
    }

    /// Read one item of the `Inputs`, `Outputs`, or `Consequences` `option`
    /// into the parameters, options, returns, or consequences of `markdown`,
    /// as Markdown: `"n" => ZZ`, `"n" => {ZZ, "description"}`, an optional
    /// argument `Limit => …`, or plain hypertext.
    fn read_hypertext_item(
        &mut self,
        item: SyntaxNode<'_>,
        option: DocumentOption,
        markdown: &mut NodeMarkdown,
    ) {
        let parts = item
            .is_option_assignment()
            .then(|| item.binary_parts())
            .flatten()
            .map(|parts| (parts.left, parts.right));
        let (rendered, names_option) = match parts {
            None => (HypertextMarkdown::render(item).trim().to_string(), false),
            Some((name, description)) => {
                if matches!(option, DocumentOption::Inputs | DocumentOption::Outputs) {
                    if let Some(bytes) = name.string_literal_inner_bytes() {
                        self.documentation.markup.push(DocumentationMarkup {
                            bytes,
                            kind: DocumentationMarkupKind::ItemName,
                        });
                    }
                }
                self.reference_code_names(description);
                let parts = if description.is::<List>() {
                    description.collection_elements().collect()
                } else {
                    vec![description]
                };
                let description = parts
                    .into_iter()
                    .map(|part| {
                        if part.is::<Symbol>() {
                            format!("`{}`", part.text())
                        } else {
                            HypertextMarkdown::render(part)
                        }
                    })
                    .collect::<Vec<_>>();
                let rendered_name = if name.is_string_literal() {
                    HypertextMarkdown::string_value(name)
                } else {
                    HypertextMarkdown::link_text(name)
                };
                let (item_type, text) = description.split_first().map_or_else(
                    || (String::new(), String::new()),
                    |(item_type, text)| (item_type.clone(), text.join(" ")),
                );
                let item = NodeMarkdown::item(Some(&rendered_name), [item_type, text]);
                (item, name.is::<Symbol>())
            }
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
    fn read_hypertext_links(&mut self, list: SyntaxNode<'_>, page: &mut HypertextPage) {
        for node in list.descendants() {
            if HypertextMarkdown::example_argument(node).is_some() {
                self.documentation
                    .example_ranges
                    .push(node.start_byte()..node.end_byte());
            }
            if let Some(key) = HypertextMarkdown::link_key(node) {
                self.reference_code_names(key);
                self.record_key(key);
            } else if let Some(argument) = HypertextMarkdown::example_argument(node) {
                page.examples
                    .extend(Self::example_fragments(argument, self.source, None));
            }
        }
    }

    /// The code fragments of an `EXAMPLE` argument: a string, a list of
    /// strings, or `lines` of a string, each nonblank line of which is one
    /// input. A plain string's escapes would shift every position, so only
    /// one written without them maps back to the source.
    pub(super) fn example_fragments(
        argument: SyntaxNode<'_>,
        source: &S,
        origin: Option<ParsedOrigin>,
    ) -> Vec<CodeFragment> {
        let strings = if argument.is::<List>() {
            argument.collection_elements().collect()
        } else if argument.applied_symbol_name() == Some("lines") {
            argument
                .binary_parts()
                .map(|parts| parts.right)
                .into_iter()
                .collect()
        } else {
            vec![argument]
        };
        let mut fragments = Vec::new();
        for string in strings {
            let Some(body) = string.string_literal_inner_bytes() else {
                continue;
            };
            let body = origin.map_or(body.clone(), |origin| origin.source_range(body));
            let text = &source.text()[body.clone()];
            if string.is::<StringLiteral>() && text.contains('\\') {
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
