//! Constructor shorthand for callable metaclasses.

use m2_syn::nodes::Expr;

use crate::analysis::{Analysis, TypeChecker};
use crate::object_registry::ObjectRegistry;
use crate::source::SyntaxSource;
use crate::syntax::{ClauseSyntax, ExpressionSyntax};
use crate::typesystem::TypeRole;

use super::unary_call::UnarySymbolCall;
use super::RewriteKind;

/// A plain `new T from value` expression with a named target type.
#[derive(Debug, Clone, Copy)]
pub struct ConstructorCall<'ast> {
    /// Type object whose metaclass supplies call syntax.
    pub class: &'ast Expr,
    /// Data passed to the constructor, retaining its original grouping.
    pub value: &'ast Expr,
}

impl<'ast> ConstructorCall<'ast> {
    /// Recognizes construction with `from` and without an explicit `of` clause.
    /// Computed target types and statement-bearing class holders stay unchanged.
    pub fn of(expression: &'ast Expr) -> Option<Self> {
        let Expr::New(construction) = expression else {
            return None;
        };
        if construction.parent_class.is_some() {
            return None;
        }
        UnarySymbolCall::grouped_value(&construction.class)?.symbol_name()?;
        let class = construction.class.as_ref();
        Some(Self {
            class,
            value: construction.instance.as_ref()?.clause_value(),
        })
    }

    /// Selects the rule for a proven callable metaclass, including source-defined
    /// metaclass inheritance. Header and wrapper shorthand only forwards list or
    /// sequence inputs directly; other wrapper inputs would acquire an extra list.
    pub fn kind(
        &self,
        analysis: &Analysis,
        source: &(impl SyntaxSource + ?Sized),
        registry: &ObjectRegistry,
    ) -> Option<RewriteKind> {
        let position = source.position_for_syntax(self.class)?;
        let knowledge = registry.at(position);
        let class = analysis.infer_expression_static_type(self.class, source, &knowledge)?;
        let checker = TypeChecker::new(analysis, registry);
        let families = [
            (
                TypeRole::SelfInitializingType,
                RewriteKind::SelfInitializingConstructor,
            ),
            (TypeRole::HeaderType, RewriteKind::HeaderConstructor),
            (TypeRole::WrapperType, RewriteKind::WrapperConstructor),
        ];
        let (_, kind) = families.into_iter().find(|(role, _)| {
            checker.is_subtype(&class, &role.object_name(), position, &knowledge)
        })?;
        if kind == RewriteKind::SelfInitializingConstructor {
            return Some(kind);
        }
        let input = analysis.infer_expression_static_type(self.value, source, &knowledge)?;
        [TypeRole::List, TypeRole::Sequence]
            .into_iter()
            .any(|role| checker.is_subtype(&input, &role.object_name(), position, &knowledge))
            .then_some(kind)
    }
}
