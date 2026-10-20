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
    DocumentationCollector, DocumentationFaultKind, DocumentationKey, DocumentationNode,
    DocumentedSubject, NodeMarkdown,
};
use crate::node_metadata::M2Node;
use crate::package_index::Macaulay2Conventions;
use crate::source::SourceNavigation;
use crate::source_package::CodeFragment;

/// The options `document` accepts in its list (`documentOptions` in Core's
/// `document.m2`). The description has no option: it is every element of the
/// list that is not an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, Display, EnumIter)]
pub enum DocumentOption {
    Key,
    Headline,
    BaseFunction,
    Usage,
    Inputs,
    Outputs,
    Consequences,
    Acknowledgement,
    Contributors,
    References,
    Citation,
    Caveat,
    SeeAlso,
    Subnodes,
    SourceCode,
    ExampleFiles,
}

/// The hypertext constructors of the `Text` package and Core that the
/// Markdown rendering distinguishes. Any other function applied in hypertext
/// renders as its Macaulay2 source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[allow(clippy::upper_case_acronyms)]
enum MarkupFunction {
    TO,
    TOH,
    TO2,
    TT,
    CODE,
    KBD,
    SAMP,
    VAR,
    M2CODE,
    BOLD,
    STRONG,
    EM,
    ITALIC,
    HREF,
    PARA,
    DIV,
    BLOCKQUOTE,
    INDENT,
    SPAN,
    SMALL,
    SUB,
    SUP,
    LABEL,
    TEX,
    LITERAL,
    UL,
    OL,
    MENU,
    DL,
    LI,
    DT,
    DD,
    SUBSECTION,
    HEADER1,
    HEADER2,
    HEADER3,
    HEADER4,
    HEADER5,
    HEADER6,
    PRE,
    EXAMPLE,
    BR,
    HR,
    COMMENT,
    IMG,
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
    fn rendering(self) -> MarkupRendering {
        use MarkupFunction::*;
        match self {
            TO | TOH | TO2 => MarkupRendering::Link,
            TT | CODE | KBD | SAMP | VAR | M2CODE => MarkupRendering::Code,
            BOLD | STRONG => MarkupRendering::Bold,
            EM | ITALIC => MarkupRendering::Italic,
            HREF => MarkupRendering::Hyperlink,
            PARA | DIV | BLOCKQUOTE | INDENT => MarkupRendering::Block,
            SPAN | SMALL | SUB | SUP | LABEL | TEX | LITERAL | LI | DT | DD => {
                MarkupRendering::Inline
            }
            UL | OL | MENU | DL => MarkupRendering::List,
            SUBSECTION | HEADER1 | HEADER2 | HEADER3 | HEADER4 | HEADER5 | HEADER6 => {
                MarkupRendering::Heading
            }
            PRE => MarkupRendering::Preformatted,
            EXAMPLE => MarkupRendering::Example,
            BR => MarkupRendering::LineBreak,
            HR => MarkupRendering::Rule,
            COMMENT | IMG => MarkupRendering::Hidden,
        }
    }
}

/// Renders hypertext — strings and markup applications — as Markdown.
pub(super) struct HypertextMarkdown;

impl HypertextMarkdown {
    /// The hypertext `node` builds, as Markdown.
    pub(super) fn render(node: M2Node<'_>) -> String {
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
                    NodeMarkdown::fenced_code("m2", lines.join("\n").trim_end())
                )
            }
            MarkupRendering::LineBreak => "  \n".to_string(),
            MarkupRendering::Rule => "\n\n---\n\n".to_string(),
            MarkupRendering::Hidden => String::new(),
        }
    }

    /// The text of a link to the documentation key `key`, or of a link
    /// `key` builds itself, such as `TO2(f, "text")`.
    pub(super) fn link_text(key: M2Node<'_>) -> String {
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

    /// The markup function `node` applies, and its argument.
    fn markup_application(node: M2Node<'_>) -> Option<(MarkupFunction, M2Node<'_>)> {
        let function = node.applied_symbol_name()?.parse().ok()?;
        Some((function, node.child_by_field_name("right")?))
    }

    /// The arguments of a markup application: the elements of a sequence or
    /// parenthesized argument, or the argument itself, a list among them.
    /// Options, such as `"class" => "x"`, set attributes and are skipped.
    fn arguments(argument: M2Node<'_>) -> Vec<M2Node<'_>> {
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
    fn string_value(node: M2Node<'_>) -> String {
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
    keys: Vec<DocumentationKey>,
    markdown: NodeMarkdown,
    examples: Vec<CodeFragment>,
    seen: Vec<DocumentOption>,
    description: String,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read the hypertext `list` passed to the `document` `call`.
    pub(super) fn read_hypertext_list(&mut self, call: M2Node<'_>, list: M2Node<'_>) {
        let mut page = HypertextPage::default();
        for element in list.collection_elements() {
            if !element.is_option_assignment() {
                page.description
                    .push_str(&HypertextMarkdown::render(element));
                continue;
            }
            let (Some(option), Some(value)) = (
                element.child_by_field_name("left"),
                element.child_by_field_name("right"),
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
            self.read_document_option(&mut page, known, value);
        }
        self.read_hypertext_links(list, &mut page);

        if !page.seen.contains(&DocumentOption::Key) {
            let anchor = call.child_by_field_name("left").unwrap_or(call);
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
        value: M2Node<'_>,
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
                    page.keys.push(DocumentationKey {
                        range: self.source.range_for_node(key),
                        subject: DocumentedSubject::of_expression(key),
                    });
                }
            }
            Headline => {
                if value.is_string_literal() {
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
                let items = elements()
                    .into_iter()
                    .map(|item| self.hypertext_item(item))
                    .collect::<Vec<_>>();
                match option {
                    Inputs => page.markdown.inputs.extend(items),
                    Outputs => page.markdown.outputs.extend(items),
                    _ => page.markdown.consequences.extend(items),
                }
            }
            SeeAlso => {
                self.reference_code_names(value);
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

    /// One `Inputs`, `Outputs`, or `Consequences` item as Markdown: `"n" =>
    /// ZZ`, `"n" => {ZZ, "description"}`, an optional argument `Limit => …`,
    /// or plain hypertext.
    fn hypertext_item(&mut self, item: M2Node<'_>) -> String {
        if !item.is_option_assignment() {
            return HypertextMarkdown::render(item).trim().to_string();
        }
        let (Some(name), Some(description)) = (
            item.child_by_field_name("left"),
            item.child_by_field_name("right"),
        ) else {
            return HypertextMarkdown::render(item);
        };
        self.reference_code_names(description);
        let name = HypertextMarkdown::link_text(name);
        let parts = if description.is::<List>() {
            description.collection_elements().collect()
        } else {
            vec![description]
        };
        let rendered = parts
            .into_iter()
            .map(|part| {
                if part.is::<Symbol>() {
                    format!("`{}`", part.text())
                } else {
                    HypertextMarkdown::render(part).trim().to_string()
                }
            })
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        match rendered.split_first() {
            Some((first, rest)) if !rest.is_empty() => {
                format!("{name}, {first} — {}", rest.join(" "))
            }
            Some((first, _)) => format!("{name}, {first}"),
            None => name,
        }
    }

    /// Record the keys `TO`, `TOH`, and `TO2` link to anywhere in `list` as
    /// names the documentation evaluates, and the code of its `EXAMPLE`s as
    /// the page's examples.
    fn read_hypertext_links(&mut self, list: M2Node<'_>, page: &mut HypertextPage) {
        for node in list.descendants() {
            let Some((function, argument)) = HypertextMarkdown::markup_application(node) else {
                continue;
            };
            match function {
                MarkupFunction::TO | MarkupFunction::TOH | MarkupFunction::TO2 => {
                    if let Some(key) = HypertextMarkdown::arguments(argument).first() {
                        self.reference_code_names(*key);
                    }
                }
                MarkupFunction::EXAMPLE => page.examples.extend(self.example_fragments(argument)),
                _ => {}
            }
        }
    }

    /// The code fragments of an `EXAMPLE` argument: a string, a list of
    /// strings, or `lines` of a string, each nonblank line of which is one
    /// input. A plain string's escapes would shift every position, so only
    /// one written without them maps back to the source.
    fn example_fragments(&self, argument: M2Node<'_>) -> Vec<CodeFragment> {
        let strings = if argument.is::<List>() {
            argument.collection_elements().collect()
        } else if argument.applied_symbol_name() == Some("lines") {
            argument.child_by_field_name("right").into_iter().collect()
        } else {
            vec![argument]
        };
        let mut fragments = Vec::new();
        for string in strings {
            let Some(body) = string.string_literal_inner_bytes() else {
                continue;
            };
            let text = &self.source.text()[body.clone()];
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
                    fragments.push(CodeFragment::new(start..end, self.source));
                }
            }
        }
        fragments
    }
}
