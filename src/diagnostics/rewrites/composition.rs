//! Typed recognition of composed lambdas and semantic checks for mapping them.

use m2_syn::nodes::{Expr, ExprLambda, ExprOp, ExprOpBin, ExprOpBinAdj};

use crate::analysis::Analysis;
use crate::object_registry::ObjectRegistry;
use crate::source::SyntaxSource;
use crate::syntax::ExpressionSyntax;
use crate::typesystem::{TypeKnowledge, TypeRole};

use super::unary_call::UnarySymbolCall;

/// Standard mapping function whose two-argument form has operator notation.
const APPLY: &str = "apply";

/// `apply(collection, x -> outer(inner(x)))` with symbol-valued functions.
/// Restricting the function heads avoids hoisting computed callees out of the
/// callback, where they would be evaluated once instead of for each element.
#[derive(Debug, Clone, Copy)]
pub struct MappingComposition<'ast> {
    /// Original mapping call, used to check whether `apply` is shadowed.
    call: &'ast ExprOpBinAdj,
    /// Input traversed by `apply` and by `/`.
    pub collection: &'ast Expr,
    /// Function applied after the inner function.
    pub outer: &'ast Expr,
    /// Function receiving each input element.
    pub inner: &'ast Expr,
}

impl<'ast> MappingComposition<'ast> {
    /// Recognizes two unary calls forwarding a callback's sole parameter.
    /// Rejects extra arguments, multiple parameters, and use of the parameter
    /// as a function head, since that binding disappears in the replacement.
    pub fn of(expression: &'ast Expr) -> Option<Self> {
        if !matches!(expression, Expr::Op(ExprOp::Bin(ExprOpBin::Adj(_)))) {
            return None;
        }
        let call = UnarySymbolCall::application(expression)?;
        if call.left_operand.symbol_name()? != APPLY {
            return None;
        }
        let arguments = UnarySymbolCall::grouped_value(&call.right_operand)?.arguments();
        let [Some(collection), Some(callback)] = arguments.as_slice() else {
            return None;
        };
        let composition = LambdaComposition::of(UnarySymbolCall::grouped_value(callback)?)?;
        Some(Self {
            call,
            collection,
            outer: composition.outer,
            inner: composition.inner,
        })
    }

    /// Whether the call uses standard `apply` with a non-count input and
    /// lowercase function names. PascalCase callees are excluded as type
    /// constructors by convention; no proof of a `Function` type is required.
    pub fn is_applicable(
        &self,
        analysis: &Analysis,
        source: &(impl SyntaxSource + ?Sized),
        registry: &ObjectRegistry,
    ) -> bool {
        let Some(position) = source.position_for_syntax(self.call) else {
            return false;
        };
        let knowledge = registry.at(position);
        if analysis
            .infer_expression_static_type(self.collection, source, &knowledge)
            .is_some_and(|ty| knowledge.has_type_role(&ty, TypeRole::Integer))
        {
            // `apply(n, f)` traverses `0 ..< n`; integers do not support `/ f`.
            return false;
        }
        analysis
            .visible_source_binding_at(APPLY, position, &knowledge)
            .is_none()
            && [self.outer, self.inner]
                .into_iter()
                .all(UnarySymbolCall::has_function_name)
    }
}

/// Two unary calls forwarding a variadic lambda's sole parameter.
/// Named callees avoid hoisting computed expressions out of the callback.
#[derive(Debug, Clone, Copy)]
pub struct LambdaComposition<'ast> {
    /// Function applied to the inner call's result.
    pub outer: &'ast Expr,
    /// Function receiving the forwarded parameter.
    pub inner: &'ast Expr,
}

impl<'ast> LambdaComposition<'ast> {
    /// Recognizes `x -> outer(inner x)` with statement-free grouping.
    /// Fixed parameter lists unpack sequences differently and are excluded,
    /// as are extra arguments and callees referring to the removed parameter.
    pub fn of(expression: &'ast Expr) -> Option<Self> {
        let Expr::Lambda(ExprLambda::Variadic(lambda)) = expression else {
            return None;
        };
        let outer = UnarySymbolCall::of(&lambda.body)?;
        let inner = UnarySymbolCall::of(outer.argument)?;
        let parameter = lambda.argument.text();
        if inner.argument.symbol_name()? != parameter
            || outer.function.symbol_name()? == parameter
            || inner.function.symbol_name()? == parameter
        {
            return None;
        }
        Some(Self {
            outer: outer.function,
            inner: inner.function,
        })
    }

    /// Whether both callees follow the lowercase function naming convention.
    /// PascalCase names may denote constructors rather than functions.
    pub fn has_function_names(self) -> bool {
        [self.outer, self.inner]
            .into_iter()
            .all(UnarySymbolCall::has_function_name)
    }
}
