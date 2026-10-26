//! The layout every hover shares, after rust-analyzer's: the Macaulay2 code
//! the name stands for, a line of facts about it, what the hovered call site
//! narrows it to, and its documentation below a rule.
//!
//! ````markdown
//! ```macaulay2
//! isPrime(ZZ) -> Boolean
//! ```
//! `MethodFunctionWithOptions` · package `Core`
//!
//! **Also possible for this call**
//! ```macaulay2
//! isPrime(RingElement) -> Boolean
//! ```
//!
//! ---
//!
//! The documentation, as Macaulay2 publishes it.
//! ````
//!
//! Code is fenced as Macaulay2, so editors highlight it as code.

use std::fmt::{self, Display, Formatter};

use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use crate::macaulay2::Macaulay2Conventions;

/// The most lines of declaration code a hover shows; longer code ends in a
/// comment saying it goes on.
const MAX_CODE_LINES: usize = 12;

/// The most entries a section lists; the rest are counted.
const MAX_SECTION_ENTRIES: usize = 15;

/// The Markdown of one hover, assembled part by part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverLayout {
    code: String,
    facts: Vec<String>,
    sections: Vec<HoverSection>,
    documentation: Option<String>,
}

/// A titled part of a hover between its facts and its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HoverSection {
    title: String,
    entries: Vec<String>,
    content: SectionContent,
}

/// What the entries of a [`HoverSection`] are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SectionContent {
    /// Lines of Macaulay2 code, such as signatures, shown as one code block.
    Code,
    /// Names, shown inline as code, separated by commas.
    Names,
}

impl HoverLayout {
    /// The hover of what `code` declares or names: a declaration, a
    /// signature, or the bare name.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            facts: Vec::new(),
            sections: Vec::new(),
            documentation: None,
        }
    }

    /// Add `fact` to the line of facts under the code, such as the class of
    /// the object or the package defining it.
    pub fn with_fact(mut self, fact: impl Into<String>) -> Self {
        self.facts.push(fact.into());
        self
    }

    /// Add the section `title` of `entries`, which are `content`. A section
    /// without entries is left out.
    pub fn with_section(
        mut self,
        title: impl Into<String>,
        content: SectionContent,
        entries: impl IntoIterator<Item = String>,
    ) -> Self {
        let entries = entries.into_iter().collect::<Vec<_>>();
        if !entries.is_empty() {
            self.sections.push(HoverSection {
                title: title.into(),
                entries,
                content,
            });
        }
        self
    }

    /// Show `documentation`, Markdown, below a rule.
    pub fn with_documentation(mut self, documentation: Option<String>) -> Self {
        self.documentation = documentation
            .map(|documentation| documentation.trim().to_string())
            .filter(|documentation| !documentation.is_empty());
        self
    }

    /// The hover as the Markdown the LSP sends.
    pub fn into_hover(self) -> Hover {
        Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: self.to_string(),
            }),
            range: None,
        }
    }

    /// `code` as a Markdown code block of Macaulay2.
    pub fn fenced_code(code: &str) -> String {
        format!(
            "```{}\n{}\n```",
            Macaulay2Conventions::MARKDOWN_CODE_LANGUAGE,
            code.trim_end()
        )
    }

    /// The code shown, cut after [`MAX_CODE_LINES`] lines.
    fn shown_code(&self) -> String {
        let lines = self.code.trim().lines().collect::<Vec<_>>();
        if lines.len() <= MAX_CODE_LINES {
            return lines.join("\n");
        }
        let hidden = lines.len() - MAX_CODE_LINES;
        format!(
            "{}\n-- … {hidden} more lines",
            lines[..MAX_CODE_LINES].join("\n")
        )
    }
}

impl HoverSection {
    fn render(&self) -> String {
        let hidden = self.entries.len().saturating_sub(MAX_SECTION_ENTRIES);
        let shown = &self.entries[..self.entries.len() - hidden];
        match self.content {
            SectionContent::Code => {
                let mut code = shown.join("\n");
                if hidden > 0 {
                    code.push_str(&format!("\n-- … {hidden} more"));
                }
                format!("**{}**\n{}", self.title, HoverLayout::fenced_code(&code))
            }
            SectionContent::Names => {
                let mut names = shown
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if hidden > 0 {
                    names.push_str(&format!(", and {hidden} more"));
                }
                format!("**{}** {names}", self.title)
            }
        }
    }
}

impl Display for HoverLayout {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let mut blocks = vec![Self::fenced_code(&self.shown_code())];
        if !self.facts.is_empty() {
            blocks.push(self.facts.join(" · "));
        }
        blocks.extend(self.sections.iter().map(HoverSection::render));
        if let Some(documentation) = &self.documentation {
            blocks.push("---".to_string());
            blocks.push(documentation.clone());
        }
        // A fact line directly under the code block belongs to it.
        let (code, rest) = blocks.split_first().expect("a hover always shows code");
        write!(formatter, "{code}")?;
        for (index, block) in rest.iter().enumerate() {
            let separator = if index == 0 && !self.facts.is_empty() {
                "\n"
            } else {
                "\n\n"
            };
            write!(formatter, "{separator}{block}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_lays_out_code_facts_sections_and_documentation() {
        let hover = HoverLayout::new("isPrime(ZZ) -> Boolean")
            .with_fact("`MethodFunction`")
            .with_fact("package `Core`")
            .with_section(
                "Also possible for this call",
                SectionContent::Code,
                ["isPrime(Ideal) -> Boolean".to_string()],
            )
            .with_section("Option of", SectionContent::Names, Vec::new())
            .with_documentation(Some("Whether a number is prime.\n".to_string()));

        assert_eq!(
            hover.to_string(),
            concat!(
                "```macaulay2\nisPrime(ZZ) -> Boolean\n```\n",
                "`MethodFunction` · package `Core`\n\n",
                "**Also possible for this call**\n```macaulay2\nisPrime(Ideal) -> Boolean\n```\n\n",
                "---\n\n",
                "Whether a number is prime.",
            )
        );
    }

    #[test]
    fn long_code_and_sections_are_cut() {
        let code = (0..20)
            .map(|line| format!("x{line} = {line}"))
            .collect::<Vec<_>>();
        let names = (0..20).map(|index| format!("f{index}"));
        let hover = HoverLayout::new(code.join("\n"))
            .with_section("Option of", SectionContent::Names, names)
            .to_string();

        assert!(hover.contains("x11 = 11\n-- … 8 more lines\n```"));
        assert!(!hover.contains("x12 = 12"));
        assert!(hover.ends_with("`f14`, and 5 more"));
    }
}
