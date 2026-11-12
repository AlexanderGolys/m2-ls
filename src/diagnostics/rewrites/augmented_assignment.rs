//! `x = x op e`, which the augmented assignment `x op= e` writes without
//! repeating the target:
//!
//! ```m2
//! prefix = prefix | "X"
//! ```
//!
//! reads `prefix |= "X"`. Only a symbol target is rewritten, and only for an
//! operator that has an augmented form: `+ - * / // % ^ | & ++ ** << >> @@ …`
//! have one, while `== < <= or and ? :` and the rest do not.

use m2_syn::nodes::{Expr, ExprAssign, ExprAssignSimple, PackedContent};

use super::RewriteSites;
use crate::analysis::InfixSyntax;
use crate::node_metadata::SyntaxSnapshot;
use crate::source::SourceNavigation;
use crate::syntax::ExpressionSyntax;

impl<'ast, Source: SourceNavigation + ?Sized> RewriteSites<'ast, '_, Source> {
    /// The augmented assignment `x op= e` that `expression`, `x = x op e`,
    /// abbreviates.
    pub(super) fn augmented_assignment(&self, expression: &'ast Expr) -> Option<String> {
        let Expr::Assign(ExprAssign::Simple(ExprAssignSimple::Global(assignment))) = expression
        else {
            return None;
        };
        let target = assignment.variable.text();
        let operands = assignment.value.infix_operands()?;
        if operands.left.symbol_name() != Some(target) {
            return None;
        }
        let augmented = format!(
            "{target} {}= {}",
            operands.operator,
            self.text(operands.right).trim()
        );
        Self::is_augmented_assignment(&augmented).then_some(augmented)
    }

    /// Whether `text` parses as a single augmented assignment. The grammar
    /// owns which operators have an augmented form, and spelling `x < y` as
    /// `x <= y` yields a comparison rather than an assignment, so the parser
    /// decides instead of a list kept here.
    fn is_augmented_assignment(text: &str) -> bool {
        let snapshot = SyntaxSnapshot::parse(text, None);
        let [cell] = snapshot.cells() else {
            return false;
        };
        cell.parse_error().is_none()
            && cell.syntax().is_some_and(|syntax| {
                matches!(
                    &syntax.content,
                    PackedContent::Expr(value)
                        if matches!(value.as_ref(), Expr::Assign(ExprAssign::Augmented(_)))
                )
            })
    }
}
