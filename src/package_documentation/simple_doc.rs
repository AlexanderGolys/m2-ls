//! The SimpleDoc format, read exactly as the `SimpleDoc` package reads it.
//!
//! A SimpleDoc string is an outline. Each line is stripped of its indentation,
//! which is measured in columns, a tab advancing to the next multiple of 8;
//! lines whose text starts with `--` are dropped as comments, and blank lines
//! count as indented infinitely. A section starts at each line indented no
//! deeper than every line before it in the enclosing section, and runs until
//! the next one: its first line is the section keyword and the lines after it
//! are its content, read by the keyword's own rules. The keywords a section
//! may start with depend on where it is:
//!
//! | Inside | Keywords |
//! | --- | --- |
//! | the string, or a `Node` | `Node`, `Key`, `Headline`, `Usage`, `Inputs`, `Outputs`, `Consequences`, `Description`, `Synopsis`, `Acknowledgement`, `Contributors`, `References`, `Citation`, `Caveat`, `SeeAlso`, `Subnodes`, `SourceCode`, `ExampleFiles` |
//! | `Description` | `Example`, `CannedExample`, `Text`, `Tree`, `Pre`, `Code` |
//! | `Synopsis` | `Heading`, `BaseFunction`, `Usage`, `Inputs`, `Outputs`, `Consequences`, `Description` |
//! | `Consequences` | `Item` |
//!
//! A string either is one page — its sections are the page's — or lists
//! pages, each a `Node` section; mixing the two is an error.
//!
//! Text sections (`Text`, `Caveat`, …) are paragraphs separated by blank
//! lines, in which `@…@` encloses a Macaulay2 expression evaluated, inside
//! parentheses, in the package — usually hypertext such as `@TO f@`; `\@` is a
//! literal `@`. Keys, `SeeAlso` and `Subnodes` entries, `SourceCode` methods,
//! input and output types, and `Code` sections are Macaulay2 expressions
//! evaluated in the package too. `Example` lines are code run in the session
//! that builds the documentation, which has loaded the package. `Usage` lines
//! and `CannedExample`s are code SimpleDoc only shows; their tokens are marked
//! like those of the code it evaluates.

use std::ops::Range;

use strum::{Display, EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};
use tower_lsp::lsp_types::Range as TextRange;

/// The TeX command `TEX` turns into a hyperlink, `\url{…}`.
const TEX_URL_COMMAND: &str = "\\url{";
/// Source spelling of the same command inside a quoted M2 string.
const TEX_QUOTED_URL_COMMAND: &str = "\\\\url{";

/// The delimiters of TeX mathematics, display before inline, as KaTeX reads
/// them in Macaulay2's documentation.
const TEX_MATH_DELIMITERS: [(&str, &str); 4] =
    [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")];

use super::{
    hypertext::HypertextMarkdown, DocumentationCollector, DocumentationFaultKind, DocumentationKey,
    DocumentationMarkup, DocumentationMarkupKind, DocumentationNode, DocumentedSubject,
    EvaluatedExpression, NodeMarkdown, TextStyle,
};
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::SyntaxNode;
use crate::source::{ByteRange, SourceNavigation};
use crate::source_package::CodeFragment;

/// Every keyword that starts a SimpleDoc section, wherever it may appear.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, EnumString, IntoStaticStr, Display, EnumIter,
)]
pub enum SimpleDocKeyword {
    Acknowledgement,
    BaseFunction,
    CannedExample,
    Caveat,
    Citation,
    Code,
    Consequences,
    Contributors,
    Description,
    Example,
    ExampleFiles,
    Headline,
    Heading,
    Inputs,
    Item,
    Key,
    Node,
    Outputs,
    Pre,
    References,
    SeeAlso,
    SourceCode,
    Subnodes,
    Synopsis,
    Text,
    Tree,
    Usage,
}

/// The places a SimpleDoc section may appear, each accepting its own
/// keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionContext {
    /// The top of the string, or inside a `Node`: the sections of a page.
    Page,
    /// Inside `Description`.
    Description,
    /// Inside `Synopsis`.
    Synopsis,
    /// Inside `Consequences`.
    Consequences,
}

impl SectionContext {
    /// Whether a section of this context may start with `keyword`.
    fn accepts(self, keyword: SimpleDocKeyword) -> bool {
        use SimpleDocKeyword::*;
        match self {
            Self::Page => matches!(
                keyword,
                Node | Key
                    | Headline
                    | Usage
                    | Inputs
                    | Outputs
                    | Consequences
                    | Description
                    | Synopsis
                    | Acknowledgement
                    | Contributors
                    | References
                    | Citation
                    | Caveat
                    | SeeAlso
                    | Subnodes
                    | SourceCode
                    | ExampleFiles
            ),
            Self::Description => {
                matches!(keyword, Example | CannedExample | Text | Tree | Pre | Code)
            }
            Self::Synopsis => matches!(
                keyword,
                Heading | BaseFunction | Usage | Inputs | Outputs | Consequences | Description
            ),
            Self::Consequences => keyword == Item,
        }
    }

    /// The keywords a section of this context may start with, sorted as
    /// SimpleDoc lists them in its error message.
    fn keywords(self) -> Vec<SimpleDocKeyword> {
        SimpleDocKeyword::iter()
            .filter(|keyword| self.accepts(*keyword))
            .collect()
    }
}

impl SimpleDocKeyword {
    /// Whether a page may give the section only once: every section becomes
    /// one `document` option, except `Description` and `Synopsis`, which add
    /// to the description, and `Node`, which starts a page.
    fn is_single_option(self) -> bool {
        !matches!(self, Self::Description | Self::Synopsis | Self::Node)
    }
}

/// One line of a SimpleDoc string as `makeTextline` reads it.
#[derive(Debug, Clone)]
struct SimpleDocLine {
    /// The text of the line without its surrounding whitespace.
    text: ByteRange,
    /// The columns of indentation; `None` for a blank line, which SimpleDoc
    /// treats as infinitely indented.
    indent: Option<usize>,
}

impl SimpleDocLine {
    /// The lines of the SimpleDoc string at `body` of `text`, without the
    /// comment lines SimpleDoc drops.
    fn read_all(text: &str, body: ByteRange) -> Vec<Self> {
        let mut lines = Vec::new();
        let mut offset = body.start;
        for segment in text[body].split_inclusive('\n') {
            let start = offset;
            offset += segment.len();
            let line = segment.trim_end_matches(['\n', '\r']);
            let content = line.trim_start();
            if content.starts_with("--") {
                continue;
            }
            let content = content.trim_end();
            let leading = line.len() - line.trim_start().len();
            let indent = (!content.is_empty()).then(|| {
                let tab = Macaulay2Conventions::SIMPLE_DOC_TAB_WIDTH;
                line[..leading].chars().fold(0, |level, character| {
                    if character == '\t' {
                        tab * ((level + tab) / tab)
                    } else {
                        level + 1
                    }
                })
            });
            lines.push(Self {
                text: start + leading..start + leading + content.len(),
                indent,
            });
        }
        lines
    }

    fn is_blank(&self) -> bool {
        self.indent.is_none()
    }

    /// The sections of `lines`, as index ranges into them, the way
    /// `splitByIndent` finds them: a section starts at each line indented no
    /// deeper than every line before it. Lines before the first section
    /// belong to none. Blank lines belong to the section around them.
    fn split_by_indent(lines: &[Self]) -> Vec<Range<usize>> {
        let mut starts = Vec::new();
        let mut minimum = i64::MAX;
        for (index, line) in lines.iter().enumerate() {
            let indent = line.indent.map_or(i64::MAX, |indent| indent as i64);
            if minimum.saturating_add(1) <= indent {
                continue;
            }
            minimum = indent;
            starts.push(index);
        }
        starts
            .iter()
            .enumerate()
            .map(|(ordinal, start)| *start..starts.get(ordinal + 1).copied().unwrap_or(lines.len()))
            .collect()
    }
}

/// One section of a SimpleDoc string: its keyword line and its content lines.
#[derive(Debug, Clone, Copy)]
struct Section<'lines> {
    keyword_line: &'lines SimpleDocLine,
    content: &'lines [SimpleDocLine],
}

impl Section<'_> {
    /// The content lines that are not blank.
    fn nonblank(&self) -> impl Iterator<Item = &SimpleDocLine> {
        self.content.iter().filter(|line| !line.is_blank())
    }
}

/// What one page collects while its sections are read.
#[derive(Debug, Default)]
struct PageReading {
    keys: Vec<DocumentationKey>,
    markdown: NodeMarkdown,
    examples: Vec<CodeFragment>,
    seen: Vec<SimpleDocKeyword>,
    has_usage: bool,
    /// The first `Inputs` or `Outputs` keyword, which needs a `Usage`.
    described_items: Option<TextRange>,
    /// The positions in the document's markup of the marks of the page's
    /// `Usage` lines.
    usage_markup: Range<usize>,
    /// The names the page's `Inputs` and `Outputs` give what they describe.
    item_names: Vec<String>,
}

impl<S: SourceNavigation + ?Sized> DocumentationCollector<'_, S> {
    /// Read the SimpleDoc string at `body` passed to `call`.
    pub(super) fn read_simple_doc(&mut self, call: SyntaxNode<'_>, body: ByteRange) {
        let lines = SimpleDocLine::read_all(self.source.text(), body.clone());
        let sections = self.sections(&lines, SectionContext::Page);
        let nodes = sections
            .iter()
            .filter(|section| self.keyword(section) == Some(SimpleDocKeyword::Node))
            .copied()
            .collect::<Vec<_>>();
        if nodes.is_empty() {
            let callee = call.binary_parts().map(|parts| parts.left).map_or_else(
                || self.source.range_for_node(call),
                |callee| self.source.range_for_node(callee),
            );
            self.read_page(&sections, self.source.range_for_node(call), callee);
            return;
        }
        if nodes.len() < sections.len() {
            self.fault(
                self.source.range_for_bytes(body),
                DocumentationFaultKind::MixedNodes,
            );
        }
        for node in &nodes {
            let page_sections = self.sections(node.content, SectionContext::Page);
            let keyword_range = self.source.range_for_bytes(node.keyword_line.text.clone());
            let end = node
                .content
                .iter()
                .rev()
                .find(|line| !line.is_blank())
                .map_or(node.keyword_line.text.end, |line| line.text.end);
            let range = self
                .source
                .range_for_bytes(node.keyword_line.text.start..end);
            self.read_page(&page_sections, range, keyword_range);
        }
    }

    /// The keyword `section` starts with, when it is one.
    fn keyword(&self, section: &Section<'_>) -> Option<SimpleDocKeyword> {
        self.source.text()[section.keyword_line.text.clone()]
            .parse()
            .ok()
    }

    /// The sections of `lines` in `context`. A keyword the context does not
    /// accept is a fault, and its section is dropped; an accepted one is
    /// highlighted.
    fn sections<'lines>(
        &mut self,
        lines: &'lines [SimpleDocLine],
        context: SectionContext,
    ) -> Vec<Section<'lines>> {
        let mut sections = Vec::new();
        for range in SimpleDocLine::split_by_indent(lines) {
            let section = Section {
                keyword_line: &lines[range.start],
                content: &lines[range.start + 1..range.end],
            };
            let keyword_bytes = section.keyword_line.text.clone();
            match self.keyword(&section) {
                Some(keyword) if context.accepts(keyword) => {
                    self.documentation.markup.push(DocumentationMarkup {
                        bytes: keyword_bytes,
                        kind: DocumentationMarkupKind::SectionKeyword,
                    });
                    sections.push(section);
                }
                _ => self.fault(
                    self.source.range_for_bytes(keyword_bytes.clone()),
                    DocumentationFaultKind::UnknownSimpleDocKeyword {
                        keyword: self.source.text()[keyword_bytes].to_string(),
                        expected: context.keywords(),
                    },
                ),
            }
        }
        sections
    }

    /// Read the `sections` of one page, written at `range`; `anchor` is
    /// where a fault about the whole page points.
    fn read_page(&mut self, sections: &[Section<'_>], range: TextRange, anchor: TextRange) {
        let mut page = PageReading::default();
        for section in sections {
            let keyword = self
                .keyword(section)
                .expect("an accepted section starts with a keyword");
            let keyword_range = self
                .source
                .range_for_bytes(section.keyword_line.text.clone());
            if keyword.is_single_option() {
                if page.seen.contains(&keyword) {
                    self.fault(
                        keyword_range,
                        DocumentationFaultKind::RepeatedSection {
                            section: keyword.to_string(),
                        },
                    );
                }
                page.seen.push(keyword);
            }
            self.read_page_section(&mut page, keyword, section, keyword_range);
        }
        if page.keys.is_empty() && !page.seen.contains(&SimpleDocKeyword::Key) {
            self.fault(anchor, DocumentationFaultKind::MissingKey);
        }
        if let (Some(items), false) = (page.described_items, page.has_usage) {
            self.fault(items, DocumentationFaultKind::UsageRequired);
        }
        // A `Usage` line writes the inputs by the names the items give them.
        let source = self.source;
        for markup in &mut self.documentation.markup[page.usage_markup.clone()] {
            if markup.kind == DocumentationMarkupKind::Name
                && page
                    .item_names
                    .iter()
                    .any(|name| *name == source.text()[markup.bytes.clone()])
            {
                markup.kind = DocumentationMarkupKind::ItemName;
            }
        }
        self.documentation.nodes.push(DocumentationNode {
            range,
            keys: page.keys,
            markdown: page.markdown.render(),
            examples: page.examples,
        });
    }

    /// Read one section of a page or of its `Synopsis`.
    fn read_page_section(
        &mut self,
        page: &mut PageReading,
        keyword: SimpleDocKeyword,
        section: &Section<'_>,
        keyword_range: TextRange,
    ) {
        use SimpleDocKeyword::*;
        match keyword {
            Key => {
                for line in section.nonblank() {
                    let bytes = line.text.clone();
                    let range = self.source.range_for_bytes(bytes.clone());
                    if let Some(subject) = self.evaluate_simple_doc_expression(
                        bytes,
                        EvaluatedExpression::Key,
                        DocumentedSubject::of_expression,
                    ) {
                        page.keys.push(DocumentationKey { range, subject });
                    }
                }
                if section.nonblank().next().is_none() {
                    self.empty_section(keyword, keyword_range, "at least one key");
                }
            }
            Headline | Heading => {
                let Some(first) = section.content.first() else {
                    self.empty_section(keyword, keyword_range, "a single indented line");
                    return;
                };
                if let Some(extra) = section.content[1..].iter().find(|line| !line.is_blank()) {
                    self.fault(
                        self.source.range_for_bytes(extra.text.clone()),
                        DocumentationFaultKind::MultilineHeadline {
                            section: keyword.to_string(),
                        },
                    );
                }
                let text = &self.source.text()[first.text.clone()];
                if keyword == Headline {
                    let length = text.chars().count();
                    if length > Macaulay2Conventions::HEADLINE_WARNING_LENGTH {
                        self.fault(
                            self.source.range_for_bytes(first.text.clone()),
                            DocumentationFaultKind::HeadlineTooLong { length },
                        );
                    }
                    page.markdown.headline = Some(text.to_string());
                    self.documentation.markup.push(DocumentationMarkup {
                        bytes: first.text.clone(),
                        kind: DocumentationMarkupKind::Styled(TextStyle::Strong),
                    });
                } else {
                    page.markdown.description.push(format!("#### {text}"));
                }
            }
            Usage => {
                page.has_usage = true;
                if section.content.is_empty() {
                    self.empty_section(keyword, keyword_range, "at least one indented line");
                    return;
                }
                let first_mark = self.documentation.markup.len();
                for line in section.nonblank() {
                    self.mark_shown_code(line.text.clone());
                    page.markdown
                        .usage
                        .push(self.source.text()[line.text.clone()].to_string());
                }
                // A `Synopsis` may hold several `Usage` sections.
                let start = if page.usage_markup.is_empty() {
                    first_mark
                } else {
                    page.usage_markup.start
                };
                page.usage_markup = start..self.documentation.markup.len();
            }
            Inputs | Outputs => {
                page.described_items.get_or_insert(keyword_range);
                page.item_names.extend(self.item_names(section));
                self.read_items(section, keyword, &mut page.markdown);
            }
            Consequences => {
                for item in self.sections(section.content, SectionContext::Consequences) {
                    let paragraphs = self.markup(item.content);
                    page.markdown.consequences.push(paragraphs.join(" "));
                }
            }
            Description => {
                for part in self.sections(section.content, SectionContext::Description) {
                    self.read_description_part(page, &part);
                }
            }
            Synopsis => {
                for part in self.sections(section.content, SectionContext::Synopsis) {
                    let keyword = self
                        .keyword(&part)
                        .expect("an accepted section starts with a keyword");
                    let keyword_range = self.source.range_for_bytes(part.keyword_line.text.clone());
                    self.read_page_section(page, keyword, &part, keyword_range);
                }
            }
            Acknowledgement | Contributors | References | Caveat => {
                let paragraphs = self.markup(section.content);
                if !paragraphs.is_empty() {
                    page.markdown
                        .subsections
                        .push((keyword.to_string(), paragraphs.join("\n\n")));
                }
            }
            SeeAlso => {
                for line in section.nonblank() {
                    if let Some(rendered) = self.evaluate_simple_doc_expression(
                        line.text.clone(),
                        EvaluatedExpression::Key,
                        HypertextMarkdown::link_text,
                    ) {
                        page.markdown.see_also.push(rendered);
                    }
                }
            }
            Subnodes => {
                let entries = self.menu(section.content);
                page.markdown.subnodes.extend(entries);
            }
            SourceCode => {
                for line in section.nonblank() {
                    self.evaluate_simple_doc_expression(
                        line.text.clone(),
                        EvaluatedExpression::Value,
                        |_| (),
                    );
                }
            }
            BaseFunction => {
                if let Some(line) = section.nonblank().next() {
                    self.evaluate_simple_doc_expression(
                        line.text.clone(),
                        EvaluatedExpression::Value,
                        |_| (),
                    );
                }
            }
            Citation | ExampleFiles => {}
            Node | Example | CannedExample | Text | Tree | Pre | Code | Item => {
                unreachable!("a page section never starts with {keyword}")
            }
        }
    }

    /// Read one part of a `Description`.
    fn read_description_part(&mut self, page: &mut PageReading, part: &Section<'_>) {
        use SimpleDocKeyword::*;
        let keyword = self
            .keyword(part)
            .expect("an accepted section starts with a keyword");
        if let (Example | CannedExample, Some(first), Some(last)) =
            (keyword, part.content.first(), part.content.last())
        {
            self.documentation
                .example_ranges
                .push(first.text.start..last.text.end);
        }
        match keyword {
            Example => {
                let fragments = part
                    .nonblank()
                    .map(|line| CodeFragment::new(line.text.clone(), self.source))
                    .collect::<Vec<_>>();
                if !fragments.is_empty() {
                    page.markdown.description.push(NodeMarkdown::fenced_code(
                        Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE,
                        &self.reassemble(part.content),
                    ));
                }
                page.examples.extend(fragments);
            }
            CannedExample => {
                for line in part.nonblank() {
                    self.mark_shown_code(line.text.clone());
                }
                page.markdown.description.push(NodeMarkdown::fenced_code(
                    Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE,
                    &self.reassemble(part.content),
                ));
            }
            Pre => {
                for line in part.nonblank() {
                    self.documentation.markup.push(DocumentationMarkup {
                        bytes: line.text.clone(),
                        kind: DocumentationMarkupKind::Styled(TextStyle::Monospace),
                    });
                }
                page.markdown.description.push(NodeMarkdown::fenced_code(
                    "",
                    &self.reassemble(part.content),
                ));
            }
            Text => {
                let paragraphs = self.markup(part.content);
                page.markdown.description.extend(paragraphs);
            }
            Tree => {
                let entries = self.menu(part.content);
                page.markdown.description.push(entries.join("\n"));
            }
            Code => {
                let (Some(first), Some(last)) = (
                    part.nonblank().next(),
                    part.content.iter().rev().find(|line| !line.is_blank()),
                ) else {
                    return;
                };
                if let Some(rendered) = self.evaluate_simple_doc_expression(
                    first.text.start..last.text.end,
                    EvaluatedExpression::Value,
                    HypertextMarkdown::render,
                ) {
                    page.markdown.description.push(rendered);
                }
            }
            _ => unreachable!("a description part never starts with {keyword}"),
        }
    }

    /// Report that the `keyword` section at `range` lacks `expectation`.
    fn empty_section(
        &mut self,
        keyword: SimpleDocKeyword,
        range: TextRange,
        expectation: &'static str,
    ) {
        self.fault(
            range,
            DocumentationFaultKind::EmptySection {
                section: keyword.to_string(),
                expectation,
            },
        );
    }

    /// The names the items of `section`, an `Inputs` or `Outputs` section,
    /// give what they describe: the `n` of `n:ZZ`.
    fn item_names(&self, section: &Section<'_>) -> Vec<String> {
        SimpleDocLine::split_by_indent(section.content)
            .into_iter()
            .filter_map(|range| {
                let line = &section.content[range.start];
                let item_line = ItemLine {
                    text: &self.source.text()[line.text.clone()],
                    origin: line.text.start,
                };
                let [(separator_start, ":")] = item_line.separators()[..] else {
                    return None;
                };
                let name = item_line.trimmed(0..separator_start);
                (!name.is_empty()).then(|| self.source.text()[name].to_string())
            })
            .collect()
    }

    /// Read the items of `section`, an `Inputs` or `Outputs` section as
    /// `keyword` says, into the parameters, options, or returns of
    /// `markdown`, as Markdown list items. An item's first line names it and
    /// its type around one `:` or `=>`, optionally followed by `-- short
    /// description`; the lines indented below describe it. The type is an
    /// expression; so is the name before `=>`, which names an optional
    /// argument.
    fn read_items(
        &mut self,
        section: &Section<'_>,
        keyword: SimpleDocKeyword,
        markdown: &mut NodeMarkdown,
    ) {
        for range in SimpleDocLine::split_by_indent(section.content) {
            let line = &section.content[range.start];
            let item_line = ItemLine {
                text: &self.source.text()[line.text.clone()],
                origin: line.text.start,
            };
            let separators = item_line.separators();
            let [(separator_start, separator)] = separators[..] else {
                self.fault(
                    self.source.range_for_bytes(line.text.clone()),
                    DocumentationFaultKind::MalformedItem,
                );
                continue;
            };
            let name_bytes = item_line.trimmed(0..separator_start);
            let rest_start = separator_start + separator.len();
            let (type_end, abbreviation) = item_line.short_description(rest_start);
            let type_bytes = item_line.trimmed(rest_start..type_end);

            let is_option = separator == "=>";
            let name = if name_bytes.is_empty() {
                None
            } else if is_option {
                self.evaluate_simple_doc_expression(
                    name_bytes,
                    EvaluatedExpression::Value,
                    HypertextMarkdown::link_text,
                )
            } else {
                self.documentation.markup.push(DocumentationMarkup {
                    bytes: name_bytes.clone(),
                    kind: DocumentationMarkupKind::ItemName,
                });
                Some(self.source.text()[name_bytes].to_string())
            };
            let item_type = if type_bytes.is_empty() {
                None
            } else {
                self.evaluate_simple_doc_expression(
                    type_bytes,
                    EvaluatedExpression::Value,
                    HypertextMarkdown::link_text,
                )
            };
            let description = self
                .markup(&section.content[range.start + 1..range.end])
                .join(" ");
            let item = NodeMarkdown::item(
                name.as_deref(),
                [item_type.unwrap_or_default(), abbreviation, description],
            );
            match (keyword, is_option) {
                (SimpleDocKeyword::Inputs, true) => markdown.options.push(item),
                (SimpleDocKeyword::Inputs, false) => markdown.inputs.push(item),
                _ => markdown.outputs.push(item),
            }
        }
    }

    /// The paragraphs of a text section as Markdown. Its lines group into
    /// paragraphs at blank lines; a section whose first line is `Tree` is a
    /// menu instead.
    fn markup(&mut self, lines: &[SimpleDocLine]) -> Vec<String> {
        let Some(first) = lines.first() else {
            return Vec::new();
        };
        if self.source.text()[first.text.clone()] == SimpleDocKeyword::Tree.to_string() {
            return vec![self.menu(&lines[1..]).join("\n")];
        }
        let mut paragraphs = Vec::new();
        for paragraph in lines.split(SimpleDocLine::is_blank) {
            let (Some(first), Some(last)) = (paragraph.first(), paragraph.last()) else {
                continue;
            };
            let rendered = self.render_text(first.text.start..last.text.end);
            if !rendered.is_empty() {
                paragraphs.push(rendered);
            }
        }
        paragraphs
    }

    /// The SimpleDoc text at `bytes` as Markdown: lines joined by single
    /// spaces, each `@…@` expression rendered as the hypertext it evaluates
    /// to, and `\@` read as `@`.
    fn render_text(&mut self, bytes: ByteRange) -> String {
        let text = &self.source.text()[bytes.clone()];
        let mut markdown = String::new();
        let mut prose_start = 0;
        let mut opening = None;
        let mut escaped = false;
        let mut segments = Vec::new();
        for (offset, character) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '@' => match opening.take() {
                    None => {
                        segments.push(TextSegment::Prose(prose_start..offset));
                        opening = Some(offset);
                    }
                    Some(open) => {
                        segments.push(TextSegment::Expression(open + 1..offset));
                        prose_start = offset + 1;
                    }
                },
                _ => {}
            }
        }
        if let Some(open) = opening {
            self.fault(
                self.source.range_for_bytes(bytes.start + open..bytes.end),
                DocumentationFaultKind::UnmatchedAt,
            );
            segments.push(TextSegment::Prose(open..text.len()));
        } else {
            segments.push(TextSegment::Prose(prose_start..text.len()));
        }
        for segment in segments {
            match segment {
                TextSegment::Prose(range) => {
                    let prose = self.render_tex(bytes.start + range.start..bytes.start + range.end);
                    let mut previous_was_space = false;
                    for character in prose.replace("\\@", "@").chars() {
                        if character.is_whitespace() {
                            if !previous_was_space {
                                markdown.push(' ');
                            }
                            previous_was_space = true;
                        } else {
                            markdown.push(character);
                            previous_was_space = false;
                        }
                    }
                }
                TextSegment::Expression(range) => {
                    let start = bytes.start + range.start;
                    let end = bytes.start + range.end;
                    let rendered = self
                        .evaluate_simple_doc_expression(
                            start..end,
                            EvaluatedExpression::Value,
                            HypertextMarkdown::render,
                        )
                        .unwrap_or_else(|| format!("`{}`", &self.source.text()[start..end]));
                    markdown.push_str(&rendered);
                }
            }
        }
        markdown.trim().to_string()
    }

    /// The SimpleDoc prose at `bytes`, text before or between `@…@`
    /// expressions, as Markdown, with the TeX markup `TEX` converts in it:
    /// `{\bf …}`, `{\em …}`, `{\it …}`, `{\tt …}`, and `\url{…}`, whose
    /// delimiters are marked, and the dashes `---` and `--`.
    pub(super) fn render_tex(&mut self, bytes: ByteRange) -> String {
        self.render_styled_tex(bytes, None)
    }

    /// The prose at `bytes` as [`Self::render_tex`] renders it, inside a
    /// group of `style`, if any: its plain text is marked in the style,
    /// run by run, around the markup nested in it.
    fn render_styled_tex(&mut self, bytes: ByteRange, style: Option<TextStyle>) -> String {
        let source = self.source;
        let text = &source.text()[bytes.clone()];
        let mut markdown = String::new();
        let mut offset = 0;
        let mut run_start = bytes.start;
        while offset < text.len() {
            let rest = &text[offset..];
            let start = bytes.start + offset;
            let markup = if let Some(length) = Self::math_length(rest) {
                Some((length, rest[..length].to_string(), Vec::new()))
            } else if let Some((opening, close)) = [TEX_URL_COMMAND, TEX_QUOTED_URL_COMMAND]
                .iter()
                .find_map(|command| {
                    rest.starts_with(command)
                        .then(|| Self::closing_brace(&rest[command.len() - 1..]))
                        .flatten()
                        .map(|close| (command.len(), command.len() - 1 + close))
                })
            {
                Some((
                    close + 1,
                    format!("<{}>", &rest[opening..close]),
                    vec![
                        (start..start + opening, DocumentationMarkupKind::TextMarkup),
                        (
                            start + opening..start + close,
                            DocumentationMarkupKind::Styled(TextStyle::Link),
                        ),
                        (
                            start + close..start + close + 1,
                            DocumentationMarkupKind::TextMarkup,
                        ),
                    ],
                ))
            } else if let (Some((tex_style, opening)), Some(close)) =
                (TexStyle::opening(rest), Self::closing_brace(rest))
            {
                self.flush_styled_run(run_start..start, style);
                self.mark_text_markup(start..start + opening);
                let content = self.render_styled_tex(
                    start + opening..start + close,
                    Some(tex_style.text_style()),
                );
                self.mark_text_markup(start + close..start + close + 1);
                markdown.push_str(&tex_style.markdown(&content));
                offset += close + 1;
                run_start = bytes.start + offset;
                continue;
            } else {
                None
            };
            if let Some((length, rendered, marks)) = markup {
                self.flush_styled_run(run_start..start, style);
                if marks.is_empty() {
                    self.documentation.markup.push(DocumentationMarkup {
                        bytes: start..start + length,
                        kind: DocumentationMarkupKind::Styled(TextStyle::Math),
                    });
                }
                for (bytes, kind) in marks {
                    self.documentation
                        .markup
                        .push(DocumentationMarkup { bytes, kind });
                }
                markdown.push_str(&rendered);
                offset += length;
                run_start = bytes.start + offset;
                continue;
            }
            let character = rest
                .chars()
                .next()
                .expect("the rest of the text is not empty");
            markdown.push(character);
            offset += character.len_utf8();
        }
        self.flush_styled_run(run_start..bytes.end, style);
        Self::typographic_dashes(&markdown)
    }

    /// Mark the plain text at `bytes` of a group of `style`, if any.
    fn flush_styled_run(&mut self, bytes: ByteRange, style: Option<TextStyle>) {
        if let (Some(style), false) = (style, bytes.is_empty()) {
            self.documentation.markup.push(DocumentationMarkup {
                bytes,
                kind: DocumentationMarkupKind::Styled(style),
            });
        }
    }

    /// The length of the TeX mathematics `text` starts with, delimiters
    /// included: `$$…$$`, `\[…\]`, `\(…\)`, or `$…$`.
    fn math_length(text: &str) -> Option<usize> {
        TEX_MATH_DELIMITERS.iter().find_map(|(opening, closing)| {
            let inner = text.strip_prefix(opening)?;
            let close = inner.find(closing).filter(|close| *close > 0)?;
            Some(opening.len() + close + closing.len())
        })
    }

    /// The byte offset in `text` of the brace closing the one at its start,
    /// nested braces balanced.
    fn closing_brace(text: &str) -> Option<usize> {
        let mut depth = 0usize;
        for (offset, character) in text.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(offset);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// `text` with `---` as an em dash and `--` between word characters as
    /// an en dash, as `TEX` writes them.
    fn typographic_dashes(text: &str) -> String {
        let text = text.replace("---", "—");
        let characters = text.chars().collect::<Vec<_>>();
        let mut dashed = String::with_capacity(text.len());
        let mut index = 0;
        while index < characters.len() {
            let is_word = |position: Option<&char>| {
                position.is_some_and(|character| character.is_alphanumeric() || *character == '_')
            };
            if characters[index] == '-'
                && characters.get(index + 1) == Some(&'-')
                && is_word(
                    index
                        .checked_sub(1)
                        .and_then(|previous| characters.get(previous)),
                )
                && is_word(characters.get(index + 2))
            {
                dashed.push('–');
                index += 2;
            } else {
                dashed.push(characters[index]);
                index += 1;
            }
        }
        dashed
    }

    /// Mark the delimiter of TeX markup at `bytes`.
    fn mark_text_markup(&mut self, bytes: ByteRange) {
        self.documentation.markup.push(DocumentationMarkup {
            bytes,
            kind: DocumentationMarkupKind::TextMarkup,
        });
    }

    /// The entries of a `Subnodes` or `Tree` menu as Markdown list items,
    /// nested by indentation. An entry is a key linked to (`* key`, `> key`,
    /// or the bare key), text (`- text`, `:text`), or an `@…@` expression.
    fn menu(&mut self, lines: &[SimpleDocLine]) -> Vec<String> {
        let mut indents = lines
            .iter()
            .filter_map(|line| line.indent)
            .collect::<Vec<_>>();
        indents.sort_unstable();
        indents.dedup();
        let mut entries = Vec::new();
        for line in lines.iter().filter(|line| !line.is_blank()) {
            let depth = indents
                .iter()
                .position(|indent| Some(*indent) == line.indent)
                .expect("every nonblank line's indentation is listed");
            let text = &self.source.text()[line.text.clone()];
            let start = line.text.start;
            let entry =
                if let Some(rest) = text.strip_prefix("* ").or_else(|| text.strip_prefix("> ")) {
                    let rest_start = start + text.len() - rest.len();
                    self.menu_key(rest, rest_start..line.text.end)
                } else if text.starts_with("- ") {
                    self.render_text(start + 2..line.text.end)
                } else if text.starts_with(':') {
                    self.render_text(start + 1..line.text.end)
                } else {
                    self.menu_key(text, line.text.clone())
                };
            entries.push(format!("{}- {entry}", "  ".repeat(depth)));
        }
        entries
    }

    /// A menu entry linking to the key at `bytes`, whose text is `text`, or
    /// rendering the `@…@` expression it starts with.
    fn menu_key(&mut self, text: &str, bytes: ByteRange) -> String {
        if text.starts_with('@') {
            self.render_text(bytes)
        } else {
            self.evaluate_simple_doc_expression(
                bytes,
                EvaluatedExpression::Key,
                HypertextMarkdown::link_text,
            )
            .unwrap_or_default()
        }
    }

    /// The content `lines` as written, indented relative to the first line,
    /// as `reassemble` restores code and preformatted text.
    fn reassemble(&self, lines: &[SimpleDocLine]) -> String {
        let base = lines.iter().find_map(|line| line.indent).unwrap_or(0);
        let mut text = lines
            .iter()
            .map(|line| match line.indent {
                Some(indent) => format!(
                    "{}{}",
                    " ".repeat(indent.saturating_sub(base)),
                    &self.source.text()[line.text.clone()]
                ),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.truncate(text.trim_end().len());
        text
    }
}

/// A style of the TeX markup `TEX` converts in SimpleDoc prose: `{\tt …}`
/// and its kin (`convertTexTable` in Core's `hypertext.m2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
#[non_exhaustive]
enum TexStyle {
    #[strum(serialize = "bf")]
    Bold,
    #[strum(serialize = "em")]
    Emphasis,
    #[strum(serialize = "it")]
    Italic,
    #[strum(serialize = "tt")]
    Typewriter,
}

impl TexStyle {
    /// The style of the group `{\xx …}` starting `text`, with the length of
    /// its opening, the brace, command, and spaces after it.
    fn opening(text: &str) -> Option<(Self, usize)> {
        let command = text.strip_prefix("{\\")?;
        let (command, opening) = command
            .strip_prefix('\\')
            .map_or((command, 2), |command| (command, 3));
        let name_length = command
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(command.len());
        let style = command[..name_length].parse().ok()?;
        let spaces =
            command[name_length..].len() - command[name_length..].trim_start_matches(' ').len();
        Some((style, opening + name_length + spaces))
    }

    /// How the style shows text.
    fn text_style(self) -> TextStyle {
        match self {
            Self::Bold => TextStyle::Strong,
            Self::Emphasis | Self::Italic => TextStyle::Emphasis,
            Self::Typewriter => TextStyle::Monospace,
        }
    }

    /// `content` in the style, as Markdown.
    fn markdown(self, content: &str) -> String {
        match self {
            Self::Bold => format!("**{content}**"),
            Self::Emphasis | Self::Italic => format!("*{content}*"),
            Self::Typewriter => format!("`{content}`"),
        }
    }
}

/// One piece of SimpleDoc text between `@` delimiters.
enum TextSegment {
    /// Prose, as a range of the text.
    Prose(Range<usize>),
    /// The range of an `@…@` expression, without the delimiters.
    Expression(Range<usize>),
}

/// The first line of an `Inputs`/`Outputs` item: `name : type -- short
/// description`, or `name => type …` for an optional argument.
struct ItemLine<'text> {
    /// The line without its surrounding whitespace.
    text: &'text str,
    /// The source byte the line starts at.
    origin: usize,
}

impl ItemLine<'_> {
    /// The separators SimpleDoc splits the line at, `:` and `=>`: the offset
    /// and spelling of each. A well-formed item has exactly one.
    fn separators(&self) -> Vec<(usize, &'static str)> {
        let mut separators = Vec::new();
        let mut offset = 0;
        while offset < self.text.len() {
            let rest = &self.text[offset..];
            if rest.starts_with("=>") {
                separators.push((offset, "=>"));
                offset += 2;
            } else if rest.starts_with(':') {
                separators.push((offset, ":"));
                offset += 1;
            } else {
                offset += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        separators
    }

    /// Where the type after offset `type_start` ends, and the short
    /// description after the `--` ending it, if any.
    fn short_description(&self, type_start: usize) -> (usize, String) {
        match self.text[type_start..].find("--") {
            Some(comment) => (
                type_start + comment,
                self.text[type_start + comment + 2..].trim().to_string(),
            ),
            None => (self.text.len(), String::new()),
        }
    }

    /// The source bytes of the part of the line at `range`, without
    /// surrounding whitespace.
    fn trimmed(&self, range: Range<usize>) -> ByteRange {
        let part = &self.text[range.clone()];
        let start = range.start + part.len() - part.trim_start().len();
        let end = start + part.trim().len();
        self.origin + start..self.origin + end
    }
}
