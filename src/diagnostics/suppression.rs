//! Diagnostics silenced in the source itself, by a directive comment.
//!
//! ```m2
//! -- m2-ls: disable S04 unused-binding
//! -- m2-ls: disable-next-line ring-variable-naming
//! R = QQ[x0, x1]
//! ```
//!
//! A `disable` directive silences the rules it names in the whole file,
//! wherever it is written; a `disable-next-line` directive, only on the line
//! after it. Rules are named by code or by name, as in the settings; a
//! directive naming none silences every rule.

use std::collections::HashMap;

use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::Position;

use super::{DiagnosticCode, M2Diagnostic};
use crate::node_metadata::SyntaxNode;
use crate::source::SourceNavigation;

/// What starts a directive inside a line comment.
const DIRECTIVE_MARKER: &str = "m2-ls:";

/// What starts a Macaulay2 line comment.
const LINE_COMMENT: &str = "--";

/// Where a directive silences the rules it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, Display)]
#[strum(serialize_all = "kebab-case")]
#[non_exhaustive]
pub enum SuppressionScope {
    /// The whole file.
    #[strum(serialize = "disable")]
    File,
    /// The line after the directive.
    #[strum(serialize = "disable-next-line")]
    NextLine,
}

/// The rules one or more directives silence somewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SilencedRules {
    Every,
    Only(Vec<DiagnosticCode>),
}

/// Every diagnostic directive of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticSuppressions {
    /// What `disable` directives silence in the whole file.
    file: Option<SilencedRules>,
    /// What `disable-next-line` directives silence, by the line they apply to.
    lines: HashMap<u32, SilencedRules>,
}

impl SilencedRules {
    fn covers(&self, code: DiagnosticCode) -> bool {
        match self {
            Self::Every => true,
            Self::Only(codes) => codes.contains(&code),
        }
    }

    /// Both `self` and `other` silenced.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Only(mut codes), Self::Only(more)) => {
                codes.extend(more);
                Self::Only(codes)
            }
            _ => Self::Every,
        }
    }
}

impl DiagnosticSuppressions {
    /// The directives of the comments under `root`.
    pub fn collect(root: SyntaxNode<'_>, source: &(impl SourceNavigation + ?Sized)) -> Self {
        let mut suppressions = Self::default();
        for comment in root.descendants().filter(SyntaxNode::is_comment) {
            let Some((scope, rules)) = Self::directive(comment.text()) else {
                continue;
            };
            match scope {
                SuppressionScope::File => {
                    suppressions.file = Some(match suppressions.file.take() {
                        Some(silenced) => silenced.join(rules),
                        None => rules,
                    });
                }
                SuppressionScope::NextLine => {
                    let line = source.position_for_node(comment).line + 1;
                    let silenced = match suppressions.lines.remove(&line) {
                        Some(silenced) => silenced.join(rules),
                        None => rules,
                    };
                    suppressions.lines.insert(line, silenced);
                }
            }
        }
        suppressions
    }

    /// The scope and rules of the directive `comment` is, if it is one.
    /// Selectors naming no rule are left out.
    fn directive(comment: &str) -> Option<(SuppressionScope, SilencedRules)> {
        let body = comment
            .strip_prefix(LINE_COMMENT)?
            .trim_start()
            .strip_prefix(DIRECTIVE_MARKER)?;
        let mut words = body
            .split(|character: char| character.is_whitespace() || character == ',')
            .filter(|word| !word.is_empty());
        let scope = words.next()?.parse().ok()?;
        let selectors = words.collect::<Vec<_>>();
        let rules = if selectors.is_empty() {
            SilencedRules::Every
        } else {
            SilencedRules::Only(
                selectors
                    .into_iter()
                    .filter_map(DiagnosticCode::from_selector)
                    .collect(),
            )
        };
        Some((scope, rules))
    }

    /// Whether a directive silences `diagnostic`.
    pub fn suppresses(&self, diagnostic: &M2Diagnostic) -> bool {
        let code = diagnostic.rule.code;
        self.file.as_ref().is_some_and(|rules| rules.covers(code))
            || self
                .lines
                .get(&diagnostic.finding.range.start.line)
                .is_some_and(|rules| rules.covers(code))
    }

    /// The comment silencing the rule `code` in `scope`, as a quick fix
    /// writes it.
    pub fn directive_comment(scope: SuppressionScope, code: DiagnosticCode) -> String {
        format!(
            "{LINE_COMMENT} {DIRECTIVE_MARKER} {scope} {}",
            code.as_str()
        )
    }

    /// Where a quick fix writes the directive silencing a diagnostic starting
    /// at `start` in `scope`: the start of the file, or of the line.
    pub fn directive_position(scope: SuppressionScope, start: Position) -> Position {
        match scope {
            SuppressionScope::File => Position::new(0, 0),
            SuppressionScope::NextLine => Position::new(start.line, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::catalog::{RING_VARIABLE_NAMING, UNUSED_BINDING};
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    fn reported(text: &str) -> Vec<(u32, DiagnosticCode)> {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");
        let suppressions = DiagnosticSuppressions::collect(document.root_node(), &document);
        document
            .diagnostics()
            .iter()
            .filter(|diagnostic| !suppressions.suppresses(diagnostic))
            .map(|diagnostic| (diagnostic.finding.range.start.line, diagnostic.rule.code))
            .collect()
    }

    #[test]
    fn directives_silence_their_rules_in_their_scope() {
        let body = "f = () -> (\n    unused := 1;\n    2\n    )\nR = QQ[x0]\n";
        let baseline = reported(body);
        assert!(baseline.contains(&(1, UNUSED_BINDING.code)), "{baseline:?}");
        assert!(baseline
            .iter()
            .any(|(_, code)| *code == RING_VARIABLE_NAMING.code));

        let file_wide = reported(&format!("-- m2-ls: disable unused-binding\n{body}"));
        assert!(!file_wide
            .iter()
            .any(|(_, code)| *code == UNUSED_BINDING.code));
        assert!(file_wide
            .iter()
            .any(|(_, code)| *code == RING_VARIABLE_NAMING.code));

        let next_line = reported(&format!(
            "{}-- m2-ls: disable-next-line S06\nR = QQ[x0]\nS = QQ[y0]\n",
            body.replace("R = QQ[x0]\n", "")
        ));
        let ring_lines = next_line
            .iter()
            .filter(|(_, code)| *code == RING_VARIABLE_NAMING.code)
            .map(|(line, _)| *line)
            .collect::<Vec<_>>();
        assert_eq!(
            ring_lines,
            vec![6],
            "only the line after the directive is silenced"
        );

        let everything = reported(&format!("-- m2-ls: disable\n{body}"));
        assert!(everything.is_empty(), "{everything:?}");
    }

    #[test]
    fn quick_fixes_write_directives_the_reader_reads_back() {
        let comment = DiagnosticSuppressions::directive_comment(
            SuppressionScope::NextLine,
            UNUSED_BINDING.code,
        );
        assert_eq!(comment, "-- m2-ls: disable-next-line S02");
        assert_eq!(
            DiagnosticSuppressions::directive(&comment),
            Some((
                SuppressionScope::NextLine,
                SilencedRules::Only(vec![UNUSED_BINDING.code])
            ))
        );
    }
}
