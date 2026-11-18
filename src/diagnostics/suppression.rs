//! Diagnostics silenced in the source itself, by a directive comment.
//!
//! ```m2
//! -- m2-ls: disable S04 unused-binding
//! -- m2-ls: disable-next-line ring-variable-naming
//! R = QQ[x0, x1]
//! ```
//!
//! A `disable` directive silences the rules it names in the whole file,
//! wherever it is written; a `disable-next-line` directive, only in the
//! expression on the line after it: the outermost one starting there, on
//! every line it spans, or that line alone when no expression starts on it.
//! Stacked `disable-next-line` directives all apply to the line after the
//! stack:
//!
//! ```m2
//! -- m2-ls: disable-next-line T01
//! -- m2-ls: disable-next-line T05
//! (lo, hi) := concentration(
//!     source, map)
//! ```
//!
//! Rules are named by code or by name, as in the settings; a directive
//! naming none silences every rule.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;

use m2_syn::nodes::Expr;
use m2_syn::visit::{self, Visit};

use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::Position;

use super::{DiagnosticCode, M2Diagnostic};
use crate::node_metadata::SyntaxSnapshot;
use crate::source::SourceNavigation;

/// What starts a directive inside a line comment.
pub const DIRECTIVE_MARKER: &str = "m2-ls:";

/// What starts a Macaulay2 line comment.
pub const LINE_COMMENT: &str = "--";

/// Where a directive silences the rules it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, Display)]
#[strum(serialize_all = "kebab-case")]
#[non_exhaustive]
pub enum SuppressionScope {
    /// The whole file.
    #[strum(serialize = "disable")]
    File,
    /// The expression on the line after the directive and the
    /// `disable-next-line` directives stacked below it.
    #[strum(serialize = "disable-next-line")]
    NextLine,
}

/// The rules one or more directives silence somewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SilencedRules {
    /// Applies to every diagnostic rule.
    Every,
    /// Applies only to the listed diagnostic rule codes.
    Only(Vec<DiagnosticCode>),
}

/// Every diagnostic directive of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticSuppressions {
    /// What `disable` directives silence in the whole file.
    file: Option<SilencedRules>,
    /// What `disable-next-line` directives silence, by the lines of the
    /// expression they apply to.
    expressions: Vec<(RangeInclusive<u32>, SilencedRules)>,
}

/// The last line of the outermost expression starting on each line, of the
/// visited syntax.
struct ExpressionLines<'source, S: ?Sized> {
    /// Text and coordinates of the visited syntax.
    source: &'source S,
    /// The last line by the starting line.
    last_lines: HashMap<u32, u32>,
}

impl<'ast, S: SourceNavigation + ?Sized> Visit<'ast> for ExpressionLines<'_, S> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Some(bytes) = self.source.syntax_bytes(expression) {
            let last = self.source.position_for_byte(bytes.end).line;
            let first = self.source.position_for_byte(bytes.start).line;
            let entry = self.last_lines.entry(first).or_insert(last);
            *entry = last.max(*entry);
        }
        visit::visit_expr(self, expression);
    }
}

impl SilencedRules {
    /// Whether this suppression selection includes the requested diagnostic rule.
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
    /// The directives in the retained comments of `syntax`, including failed cells.
    pub fn collect(syntax: &SyntaxSnapshot, source: &(impl SourceNavigation + ?Sized)) -> Self {
        let mut suppressions = Self::default();
        let mut next_line_directives = Vec::new();
        for comment in syntax.cells().iter().flat_map(|cell| cell.trivia()) {
            if comment.kind() != m2_syn::CommentKind::Line {
                continue;
            }
            let Some(text) = source.text_for_syntax(comment) else {
                continue;
            };
            let Some(bytes) = source.syntax_bytes(comment) else {
                continue;
            };
            let mut offset = bytes.start;
            for line in text.split_inclusive('\n') {
                let line_start = offset;
                offset += line.len();
                let Some((scope, rules)) = Self::directive(line.trim_start_matches([' ', '\t']))
                else {
                    continue;
                };
                match scope {
                    SuppressionScope::File => {
                        suppressions.file = Some(match suppressions.file.take() {
                            Some(silenced) => silenced.join(rules),
                            None => rules,
                        });
                    }
                    SuppressionScope::NextLine => next_line_directives
                        .push((source.position_for_byte(line_start).line, rules)),
                }
            }
        }
        let directive_lines = next_line_directives
            .iter()
            .map(|(line, _)| *line)
            .collect::<HashSet<_>>();
        let mut targets = HashMap::<u32, SilencedRules>::new();
        for (line, rules) in next_line_directives {
            let target = (line + 1..)
                .find(|line| !directive_lines.contains(line))
                .expect("finitely many directives precede an unbounded range");
            let silenced = match targets.remove(&target) {
                Some(silenced) => silenced.join(rules),
                None => rules,
            };
            targets.insert(target, silenced);
        }
        let mut expressions = ExpressionLines {
            source,
            last_lines: HashMap::new(),
        };
        syntax.visit(&mut expressions);
        suppressions.expressions = targets
            .into_iter()
            .map(|(first, rules)| {
                let last = expressions.last_lines.get(&first).copied().unwrap_or(first);
                (first..=last, rules)
            })
            .collect();
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
            || self.expressions.iter().any(|(lines, rules)| {
                lines.contains(&diagnostic.finding.range.start.line) && rules.covers(code)
            })
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
    use crate::source::SyntaxSource;

    fn reported(text: &str) -> Vec<(u32, DiagnosticCode)> {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");
        let suppressions = DiagnosticSuppressions::collect(document.syntax(), &document);
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

    #[test]
    fn directives_follow_relocated_cells_and_survive_invalid_syntax() {
        use tower_lsp::lsp_types::{Range, TextDocumentContentChangeEvent};

        let registry = ObjectRegistry::default();
        let mut document = DocumentSnapshot::from_text(
            "x = 1\n-- m2-ls: disable-next-line S02\ny = (\n".to_string(),
            &registry,
        )
        .expect("the document retains invalid cells");
        let silenced_on = |document: &DocumentSnapshot, line: u32| {
            DiagnosticSuppressions::collect(document.syntax(), document)
                .expressions
                .iter()
                .any(|(lines, _)| lines.contains(&line))
        };
        assert!(silenced_on(&document, 2));
        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                    range_length: None,
                    text: "before = 0\n".to_string(),
                }],
                &registry,
            )
            .expect("the edit is valid");
        assert!(silenced_on(&document, 3));
        assert!(!silenced_on(&document, 2));
    }
}
