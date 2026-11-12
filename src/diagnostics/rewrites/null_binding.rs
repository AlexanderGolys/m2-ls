//! A local bound only to be tested against `null`, which `??` folds away:
//!
//! ```m2
//! (
//!     value := lookup key;
//!     if value =!= null then result = value else result = fallback
//! )
//! ```
//!
//! reads `result = lookup key ?? fallback`. The conditional without the
//! binding, `if value =!= null then result = value else result = fallback`, is
//! the plain coalescence rewrite; this one also removes the intermediate local.

use m2_syn::nodes::{Expr, Holder, PackedContent, Statement};

use super::{
    guarded_value, symbol_assignment, NullBranch, NullTest, RewriteKind, RewriteSite, RewriteSites,
    SubjectErrors,
};
use crate::source::{ByteRange, SourceNavigation};
use crate::syntax::{ClauseSyntax, ExpressionSyntax};

/// What a null binding rewrites to.
struct NullBinding {
    /// Text replacing the binding and the conditional.
    replacement: String,
    /// Bytes of the binding statement through the end of the conditional.
    extent: ByteRange,
}

/// How the conditional following a binding assigns or yields its value.
enum Outcome<'ast> {
    /// `target op value` in both branches, where `op` is `=` or `:=`.
    Assigned {
        /// The assigned name.
        target: &'ast str,
        /// The assignment operator, repeated in both branches.
        operator: &'static str,
        /// The value assigned when the local is null.
        fallback: &'ast Expr,
    },
    /// The conditional's own value, which a block returns.
    Value {
        /// The value yielded when the local is null.
        fallback: &'ast Expr,
    },
}

/// Whether `text` mentions `name` as a whole identifier.
fn mentions(text: &str, name: &str) -> bool {
    text.split(|character: char| {
        !(character.is_alphanumeric() || character == '\'' || character == '$')
    })
    .any(|word| word == name)
}

impl<'ast, Source: SourceNavigation + ?Sized> RewriteSites<'ast, '_, Source> {
    /// Records the null bindings of one parenthesized block: a statement
    /// `name := e;` directly followed by a conditional testing `name` against
    /// `null`, when nothing else mentions `name`.
    pub(super) fn null_bindings(&mut self, block: &'ast Holder) {
        for (index, binding) in block.muted.iter().enumerate() {
            let following = &block.muted[index + 1..];
            let (conditional, trailing, yields_value) = match following.split_first() {
                Some((next, rest)) => match &next.content {
                    PackedContent::Expr(conditional) => (conditional.as_ref(), rest, false),
                    PackedContent::Pack(_) => continue,
                },
                None => match block.value.as_deref() {
                    Some(conditional) => (conditional, following, true),
                    None => continue,
                },
            };
            let trailing_value = if yields_value {
                None
            } else {
                block.value.as_deref()
            };
            let Some(rewrite) =
                self.null_binding(binding, conditional, trailing, trailing_value, yields_value)
            else {
                continue;
            };
            self.covered.insert(std::ptr::from_ref(conditional));
            self.sites.push(RewriteSite {
                kind: RewriteKind::InlinedNullBinding,
                expression: conditional,
                owner: None,
                replacement: rewrite.replacement,
                extent: Some(rewrite.extent),
            });
        }
    }

    /// The rewrite of `binding` followed by `conditional`, if the binding is
    /// a null-tested local that nothing else uses. `trailing` and
    /// `trailing_value` are what follows the conditional in its block.
    fn null_binding(
        &self,
        binding: &'ast Statement,
        conditional: &'ast Expr,
        trailing: &'ast [Statement],
        trailing_value: Option<&'ast Expr>,
        yields_value: bool,
    ) -> Option<NullBinding> {
        let PackedContent::Expr(bound) = &binding.content else {
            return None;
        };
        let (local, operator, initializer) = symbol_assignment(bound)?;
        let Expr::If(test) = conditional else {
            return None;
        };
        if operator != ":=" || mentions(self.text(initializer), local) {
            return None;
        }
        let NullTest {
            subject,
            null_branch,
        } = self.null_test(test, SubjectErrors::Propagate)?;
        if subject.symbol_name() != Some(local) {
            return None;
        }
        let else_value = test.else_clause.as_ref()?.clause_value();
        let then_value = test.then_clause.clause_value();
        let (fallback_branch, local_branch) = match null_branch {
            NullBranch::Then => (then_value, else_value),
            NullBranch::Else => (else_value, then_value),
        };
        let outcome = match (
            symbol_assignment(fallback_branch),
            symbol_assignment(local_branch),
        ) {
            (Some((target, operator, fallback)), Some((repeated, repeated_operator, kept)))
                if target == repeated
                    && operator == repeated_operator
                    && target != local
                    && self.same_expression(kept, subject) =>
            {
                Outcome::Assigned {
                    target,
                    operator,
                    fallback,
                }
            }
            _ if yields_value && self.same_expression(local_branch, subject) => Outcome::Value {
                fallback: fallback_branch,
            },
            _ => return None,
        };
        let (Outcome::Assigned { fallback, .. } | Outcome::Value { fallback }) = &outcome;
        let used_elsewhere = std::iter::once(self.text(*fallback))
            .chain(trailing.iter().map(|statement| self.text(statement)))
            .chain(trailing_value.map(|value| self.text(value)))
            .any(|text| mentions(text, local));
        if used_elsewhere {
            return None;
        }
        let guarded = guarded_value(initializer);
        let subject_text = self.coalescence_subject(guarded.unwrap_or(initializer));
        let coalesced = format!("{subject_text} ?? {}", self.coalescence_operand(fallback));
        let replacement = match outcome {
            Outcome::Assigned {
                target, operator, ..
            } => format!("{target} {operator} {coalesced}"),
            Outcome::Value { .. } => coalesced,
        };
        let start = self.source.syntax_bytes(binding)?.start;
        let end = self.source.syntax_bytes(conditional)?.end;
        Some(NullBinding {
            replacement,
            extent: start..end,
        })
    }
}
