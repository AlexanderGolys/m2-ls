//! Extracts a fallback value from branches assigning the same existing symbol.
//!
//! Terminal `target = value` assignments become `value`, leaving conditionals,
//! block statements, comments, and local scopes in place. This lets an enclosing
//! null test assign once with `target = subject ?? (fallback)`. Every path must
//! assign the same symbol, and no branch may introduce a local with that name.

use m2_syn::nodes::{Expr, ExprAssign, ExprAssignSimple};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;

use super::RewriteSites;
use crate::source::{ByteRange, SourceNavigation};
use crate::syntax::{AssignmentOperator, AssignmentSyntax, ClauseSyntax};

/// Detects declarations that would change the target's binding if its final
/// assignment were moved outside a branch's lexical scope.
struct TargetDeclarations<'name> {
    /// Symbol receiving the enclosing assignment.
    target: &'name str,
    /// Whether any local declaration shadows that symbol.
    shadows_target: bool,
}

impl<'ast> Visit<'ast> for TargetDeclarations<'_> {
    /// Includes parallel local declarations and declarations nested in blocks.
    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        let parts = assignment.parts();
        self.shadows_target |= parts.operator == AssignmentOperator::Local
            && parts
                .target
                .bound_symbols()
                .iter()
                .any(|symbol| symbol.text() == self.target);
        visit::visit_expr_assign(self, assignment);
    }
}

impl<'ast, Source: SourceNavigation + ?Sized> RewriteSites<'ast, '_, Source> {
    /// Replaces terminal assignments to `target` with their values, preserving
    /// all other source text. Returns `None` for incomplete paths, other targets,
    /// local declarations of the target, and blocks without a returned value.
    pub(super) fn assigned_branch_value(&self, branch: &Expr, target: &str) -> Option<String> {
        let mut declarations = TargetDeclarations {
            target,
            shadows_target: false,
        };
        declarations.visit_expr(branch);
        if declarations.shadows_target {
            return None;
        }
        let mut assignments = Vec::new();
        self.branch_assignment_tokens(branch, target, &mut assignments)?;
        let start = self.source.syntax_bytes(branch)?.start;
        let mut replacement = self.text(branch).to_string();
        // The recursive walk visits branches in source order. Removing tokens
        // backwards keeps the earlier byte coordinates valid.
        for token in assignments.into_iter().rev() {
            replacement.replace_range(token.start - start..token.end - start, "");
        }
        Some(replacement)
    }

    /// Collects the target and `=` tokens of every terminal path. Intermediate
    /// block statements remain untouched, including their assignments and calls.
    fn branch_assignment_tokens(
        &self,
        branch: &Expr,
        target: &str,
        prefixes: &mut Vec<ByteRange>,
    ) -> Option<()> {
        match branch {
            Expr::If(conditional) => {
                self.branch_assignment_tokens(
                    conditional.then_clause.clause_value(),
                    target,
                    prefixes,
                )?;
                self.branch_assignment_tokens(
                    conditional.else_clause.as_ref()?.clause_value(),
                    target,
                    prefixes,
                )
            }
            Expr::Holder(block) => {
                self.branch_assignment_tokens(block.value.as_deref()?, target, prefixes)
            }
            _ => {
                let Expr::Assign(ExprAssign::Simple(ExprAssignSimple::Global(assignment))) = branch
                else {
                    return None;
                };
                if assignment.variable.text() != target {
                    return None;
                }
                let target_start = self.source.syntax_bytes(&assignment.variable)?.start;
                let operator_start = self.source.syntax_bytes(&assignment.operator)?.start;
                // Native token spans can include trailing comments. Remove only
                // each token's spelling so that its trivia remains in the branch.
                prefixes.push(target_start..target_start + assignment.variable.text().len());
                prefixes.push(operator_start..operator_start + <Token![=]>::SPELLING.len());
                Some(())
            }
        }
    }
}
