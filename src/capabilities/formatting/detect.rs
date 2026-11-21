//! What a document's own layout says about the rules `"detect"` follows,
//! and the style a format run applies once every `"detect"` is resolved.
//!
//! A rule is decided by the document's majority only when it shows at least
//! `detect.minSamples` occurrences and the majority reaches `detect.threshold`
//! of them; otherwise the rule's fallback applies. Only code is measured:
//! strings and comments never count.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use m2_syn::nodes::{BinaryOperator, Elements, Expr, ExprBinary, ExprSequence, ParamsPack};
use m2_syn::visit::{self, Visit};
use m2_syn::Spanned;

use super::indent::BracketBalance;
use super::layout::LayoutSource;
use super::spacing::IndexContext;
use super::{TextLine, Whitespace};
use crate::node_metadata::SyntaxSnapshot;
use crate::settings::spacing::{OperatorSpacing, Spacing};
use crate::settings::{Detectable, DetectionSettings, FormattingSettings, LambdaParamComma};
use crate::source::SourceNavigation;

/// How often a document writes something spaced and how often tight, as
/// `a + b` against `a+b`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    /// Occurrences with a space.
    spaced: usize,
    /// Occurrences without.
    tight: usize,
}

impl Tally {
    /// Count one occurrence with `gap` bytes of space, `None` when a line
    /// break intervenes and the occurrence says nothing.
    fn count(&mut self, gap: Option<usize>) {
        match gap {
            Some(0) => self.tight += 1,
            Some(_) => self.spaced += 1,
            None => {}
        }
    }

    /// The document's majority, if it says enough.
    fn majority(self, detect: &DetectionSettings) -> Option<Spacing> {
        let total = self.spaced + self.tight;
        if total < detect.min_samples {
            None
        } else if detect.threshold.reached_by(self.spaced, total) {
            Some(Spacing::Spaced)
        } else if detect.threshold.reached_by(self.tight, total) {
            Some(Spacing::Tight)
        } else {
            None
        }
    }
}

/// The layout a document mostly uses, measured before formatting: e.g. a
/// file indented in steps of 5 with `(a,b)` gives width 5 and tight commas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetectedStyle {
    /// The dominant positive indent step among [`Self::INDENT_STEPS`].
    indent_width: Option<NonZeroUsize>,
    /// Commas inside `( )`.
    comma: Option<Spacing>,
    /// Commas between lambda parameters.
    lambda_param_comma: Option<Spacing>,
    /// Each operator of `spacing.operators.detect` the document decides.
    operators: HashMap<String, Spacing>,
}

impl DetectedStyle {
    /// The indent steps a document is measured for: the historical Emacs
    /// `M2-indent-level` of 5, the current `M2-mode` default of 4, and the
    /// 2 of a few hand-indented files.
    const INDENT_STEPS: [usize; 3] = [2, 4, 5];

    /// Measure the document in `layout`, laid out as written.
    pub(super) fn measure(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        settings: &FormattingSettings,
    ) -> Self {
        let mut measure = Measure {
            layout,
            settings,
            index: IndexContext::default(),
            parentheses: Tally::default(),
            parameters: Tally::default(),
            operators: HashMap::new(),
        };
        syntax.visit(&mut measure);
        let detect = &settings.detect;
        Self {
            indent_width: Self::indent_step(syntax, layout, settings),
            comma: measure.parentheses.majority(detect),
            lambda_param_comma: measure.parameters.majority(detect),
            operators: measure
                .operators
                .into_iter()
                .filter_map(|(operator, tally)| Some((operator, tally.majority(detect)?)))
                .collect(),
        }
    }

    /// The step among [`Self::INDENT_STEPS`] that most indent increases
    /// take, each divided by the brackets the line before opens: `g(y -> (`
    /// followed by a line 8 columns in is one step of 4. Measured this way,
    /// a file indented with step `W` shows only `W`, so formatting it
    /// detects `W` again.
    fn indent_step(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        settings: &FormattingSettings,
    ) -> Option<NonZeroUsize> {
        let detect = &settings.detect;
        let text = layout.text();
        let tab_width = settings.indent.tab_width.get();
        let lines = TextLine::all(text);
        let mut steps = HashMap::<usize, usize>::new();
        let mut previous = None;
        for (row, delta) in BracketBalance::new(syntax, layout, &lines).code_rows(layout, &lines) {
            let indent = Whitespace::width(lines[row].indentation(text), tab_width);
            let step = previous
                .and_then(|previous| indent.checked_sub(previous))
                .filter(|increase| delta > 0 && increase % delta as usize == 0)
                .map(|increase| increase / delta as usize)
                .filter(|step| Self::INDENT_STEPS.contains(step));
            if let Some(step) = step {
                *steps.entry(step).or_default() += 1;
            }
            previous = Some(indent);
        }
        let total = steps.values().sum::<usize>();
        let (&step, &count) = steps.iter().max_by_key(|(step, count)| (**count, **step))?;
        (total >= detect.min_samples && detect.threshold.reached_by(count, total))
            .then(|| NonZeroUsize::new(step))
            .flatten()
    }
}

/// Tallies the spacing of the constructs `"detect"` rules measure.
struct Measure<'a> {
    /// The document as written.
    layout: &'a LayoutSource,
    /// The settings naming the measured operators.
    settings: &'a FormattingSettings,
    /// Whether the innermost bracket is an index, as in `x#(i+1)`.
    index: IndexContext,
    /// Commas inside `( )`.
    parentheses: Tally,
    /// Commas between lambda parameters.
    parameters: Tally,
    /// Operators of `spacing.operators.detect`, outside index brackets when
    /// `indexContextTight` decides those.
    operators: HashMap<String, Tally>,
}

impl Measure<'_> {
    /// The horizontal gap after each comma of `values`.
    fn commas<T: Spanned>(&self, values: &m2_syn::Punctuated<T>, tally: &mut Tally) {
        for (comma, next) in values.pairs() {
            if let (Some(comma), Some(next)) = (
                self.layout.syntax_range(comma),
                self.layout.syntax_range(next),
            ) {
                tally.count(self.layout.horizontal_gap(comma.end..next.start));
            }
        }
    }
}

impl<'ast> Visit<'ast> for Measure<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let entered = self.index.enter(expression);
        visit::visit_expr(self, expression);
        self.index.leave(entered);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        let spelling = node.op.spelling();
        let measured = !matches!(node.op, BinaryOperator::Space(_))
            && !(self.index.is_inside() && self.settings.spacing.index_context_tight)
            && self.settings.spacing.operators.binary(spelling) == Some(OperatorSpacing::Detect);
        if let (true, Some(left), Some(operator), Some(right)) = (
            measured,
            self.layout.syntax_range(node.left.as_ref()),
            self.layout.syntax_range(&node.op),
            self.layout.syntax_range(node.right.as_ref()),
        ) {
            let before = self.layout.horizontal_gap(left.end..operator.start);
            let after = self.layout.horizontal_gap(operator.end..right.start);
            if let (Some(before), Some(after)) = (before, after) {
                if (before == 0) == (after == 0) {
                    self.operators
                        .entry(spelling.to_string())
                        .or_default()
                        .count(Some(before));
                }
            }
        }
        self.index.note(node);
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_sequence(&mut self, node: &'ast ExprSequence) {
        if let Some(Elements::Pack(pack)) = &node.elems {
            let mut tally = self.parentheses;
            self.commas(&pack.elems, &mut tally);
            self.parentheses = tally;
        }
        visit::visit_expr_sequence(self, node);
    }

    fn visit_params_pack(&mut self, node: &'ast ParamsPack) {
        let mut tally = self.parameters;
        self.commas(&node.params, &mut tally);
        self.parameters = tally;
        visit::visit_params_pack(self, node);
    }
}

/// The formatting settings with every `"detect"` resolved for one document:
/// what each pass consults.
#[derive(Debug, Clone)]
pub(super) struct FormatStyle<'a> {
    /// The settings as configured.
    pub(super) settings: &'a FormattingSettings,
    /// Columns per open bracket.
    pub(super) indent_width: usize,
    /// After a comma outside lambda parameters.
    pub(super) comma: Spacing,
    /// After a comma between lambda parameters.
    pub(super) lambda_param_comma: Spacing,
    /// What the document decides for `spacing.operators.detect`.
    detected_operators: HashMap<String, Spacing>,
}

impl<'a> FormatStyle<'a> {
    /// The fallback of `lambdaParamComma = "detect"`: the 5-column Core
    /// files' `(M,N) ->`, which the median file follows.
    const LAMBDA_PARAM_COMMA_FALLBACK: Spacing = Spacing::Tight;
    /// The fallback of `comma = "detect"` and of every detected operator.
    const SPACING_FALLBACK: Spacing = Spacing::Spaced;

    /// The style for the document in `layout`, laid out as written.
    pub(super) fn for_document(
        settings: &'a FormattingSettings,
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
    ) -> Self {
        let detected = DetectedStyle::measure(syntax, layout, settings);
        let spacing = &settings.spacing;
        let comma = match spacing.comma {
            Detectable::Fixed(comma) => comma,
            Detectable::Detect => detected.comma.unwrap_or(Self::SPACING_FALLBACK),
        };
        let lambda_param_comma = match spacing.lambda_param_comma {
            Detectable::Fixed(LambdaParamComma::Space) => Spacing::Spaced,
            Detectable::Fixed(LambdaParamComma::Tight) => Spacing::Tight,
            Detectable::Fixed(LambdaParamComma::SameAsComma) => comma,
            Detectable::Detect => detected
                .lambda_param_comma
                .unwrap_or(Self::LAMBDA_PARAM_COMMA_FALLBACK),
        };
        Self {
            settings,
            indent_width: match settings.indent.width {
                Detectable::Fixed(width) => width,
                Detectable::Detect => detected
                    .indent_width
                    .unwrap_or(settings.indent.fallback_width),
            }
            .get(),
            comma,
            lambda_param_comma,
            detected_operators: detected.operators,
        }
    }

    /// How the binary `operator` is spaced, tight inside an index when
    /// `indexContextTight` says so; `None` keeps it as written.
    pub(super) fn binary_spacing(&self, operator: &str, in_index: bool) -> Option<Spacing> {
        let spacing = match self.settings.spacing.operators.binary(operator)? {
            OperatorSpacing::Fixed(spacing) => spacing,
            OperatorSpacing::Detect => self
                .detected_operators
                .get(operator)
                .copied()
                .unwrap_or(Self::SPACING_FALLBACK),
        };
        let indexed = in_index
            && self.settings.spacing.index_context_tight
            && IndexContext::ARITHMETIC.contains(&operator);
        Some(if indexed { Spacing::Tight } else { spacing })
    }
}
