//! Typed recognition and semantic checks for mapping a composed callback.

use m2_syn::nodes::{Expr, ExprLambda, ExprOp, ExprOpBin, ExprOpBinAdj};

use crate::analysis::Analysis;
use crate::object_registry::ObjectRegistry;
use crate::source::SyntaxSource;
use crate::syntax::{BoundSymbols, ExpressionSyntax, LambdaSyntax};
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
        let Expr::Lambda(callback) = UnarySymbolCall::grouped_value(callback)? else {
            return None;
        };
        // A variadic parameter can receive a Sequence as one value; a fixed
        // `(x) -> ...` parameter unpacks it and has different call semantics.
        if !matches!(callback, ExprLambda::Variadic(_)) {
            return None;
        }
        let parameters = callback.bound_symbols();
        let [parameter] = parameters.as_slice() else {
            return None;
        };
        let outer = UnarySymbolCall::of(callback.body())?;
        let inner = UnarySymbolCall::of(outer.argument)?;
        if inner.argument.symbol_name()? != parameter.text()
            || outer.function.symbol_name()? == parameter.text()
            || inner.function.symbol_name()? == parameter.text()
        {
            return None;
        }
        Some(Self {
            call,
            collection,
            outer: outer.function,
            inner: inner.function,
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
