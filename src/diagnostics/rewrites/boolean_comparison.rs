//! Redundant strict comparisons of Boolean expressions with Boolean literals.

use m2_syn::nodes::Expr;
use m2_syn::Token;

use crate::analysis::{Analysis, InfixSyntax};
use crate::object_registry::ObjectRegistry;
use crate::source::SyntaxSource;
use crate::syntax::ExpressionSyntax;
use crate::typesystem::{TypeKnowledge, TypeRole};

/// Name of the language's false value.
const BOOLEAN_FALSE: &str = "false";

/// Name of the language's true value.
const BOOLEAN_TRUE: &str = "true";

/// A strict Boolean comparison reducible to its subject or its negation.
#[derive(Debug, Clone, Copy)]
pub struct BooleanComparison<'ast> {
    /// Expression evaluated once by both the original and the replacement.
    pub subject: &'ast Expr,
    /// Boolean literal whose unshadowed value determines the rewrite.
    literal: &'ast Expr,
    /// Whether the replacement negates the subject.
    pub negated: bool,
}

impl<'ast> BooleanComparison<'ast> {
    /// Recognizes `===` and `=!=` with exactly one Boolean literal, on either
    /// side. Comparisons of two literals are left to constant folding.
    pub fn of(expression: &'ast Expr) -> Option<Self> {
        let operands = expression.infix_operands()?;
        let unequal = match operands.operator {
            <Token![===]>::SPELLING => false,
            <Token![=!=]>::SPELLING => true,
            _ => return None,
        };
        let (subject, literal, value) = match (
            Self::literal_value(operands.left),
            Self::literal_value(operands.right),
        ) {
            (Some(value), None) => (operands.right, operands.left, value),
            (None, Some(value)) => (operands.left, operands.right, value),
            _ => return None,
        };
        Some(Self {
            subject,
            literal,
            negated: value == unequal,
        })
    }

    /// Requires a Boolean subject and an unshadowed Boolean literal. Strict
    /// comparison accepts any value, while a direct condition or `not` does
    /// not: `null === true` is false, but using `null` as a condition fails.
    pub fn is_applicable(
        &self,
        analysis: &Analysis,
        source: &(impl SyntaxSource + ?Sized),
        registry: &ObjectRegistry,
    ) -> bool {
        let Some(position) = source.position_for_syntax(self.literal) else {
            return false;
        };
        let knowledge = registry.at(position);
        let Some(literal_name) = self.literal.unparenthesized().and_then(Expr::symbol_name) else {
            return false;
        };
        analysis
            .visible_source_binding_at(literal_name, position, &knowledge)
            .is_none()
            && analysis
                .infer_expression_static_type(self.subject, source, &knowledge)
                .is_some_and(|ty| knowledge.has_type_role(&ty, TypeRole::Boolean))
    }

    /// Whether a parenthesized or bare symbol names `false` or `true`.
    fn literal_value(expression: &Expr) -> Option<bool> {
        match expression.unparenthesized()?.symbol_name()? {
            BOOLEAN_FALSE => Some(false),
            BOOLEAN_TRUE => Some(true),
            _ => None,
        }
    }
}
