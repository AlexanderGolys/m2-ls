//! Type rules on generated syntax, independent of document navigation.
//!
//! Each concrete construct owns its inference rule. Generated enums only
//! delegate to their alternatives; operands remain borrowed typed expressions.
//! The context supplies positioned semantic knowledge and memoized recursion.

use m2_syn::{nodes::*, Spanned};

use crate::object_registry::OperatorForm;
use crate::typesystem::{InferredType, TypeRole};

pub mod session;
pub mod syntax;

/// Semantic operations needed by generated expression rules.
///
/// The AST borrow bounds recursive queries to the lifetime of the same syntax
/// snapshot. Implementations decide how source positions and scopes are resolved.
pub trait InferenceContext<'ast> {
    /// Infers a typed operand in the current lexical scope.
    fn expression(&self, expression: &'ast Expr) -> InferredType;
    /// Resolves an exact runtime class at the supplied syntax position.
    fn exact(&self, syntax: &dyn Spanned, class: TypeRole) -> InferredType;
    /// Resolves a class named explicitly by a new expression.
    fn named_class(&self, syntax: &dyn Spanned, name: &str) -> InferredType;
    /// Resolves a symbol through source bindings, output history, and the catalog.
    fn symbol(&self, symbol: &'ast Symbol) -> InferredType;
    /// Infers application from the callable and its typed argument expression.
    fn application(&self, application: &'ast ExprBinary) -> InferredType;
    /// Infers a field access whose key is a literal symbol.
    fn field(&self, field: &'ast ExprField) -> InferredType;
    /// Dispatches an infix operator using its two typed operands.
    fn binary(
        &self,
        syntax: &dyn Spanned,
        operator: &str,
        left: &'ast Expr,
        right: &'ast Expr,
    ) -> InferredType;
    /// Dispatches a prefix or postfix operator using its typed operand.
    fn unary(
        &self,
        syntax: &dyn Spanned,
        operator: &str,
        operand: &'ast Expr,
        form: OperatorForm,
    ) -> InferredType;
    /// Joins branch results using type ancestry visible at the branch owner.
    fn join(&self, syntax: &dyn Spanned, left: InferredType, right: InferredType) -> InferredType;
}

/// A generated syntax construct that can infer its own result type.
pub trait InferType: Spanned {
    /// Evaluates this construct's rule without erasing its operands' types.
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType;
}

/// Implements fixed-class rules for constructs whose contents do not affect their class.
macro_rules! exact_classes {
    ($($syntax:ty => $class:ident),+ $(,)?) => {$(
        impl InferType for $syntax {
            fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
                context.exact(self, TypeRole::$class)
            }
        }
    )+};
}
exact_classes! {
    FloatLiteral => Real, IntegerLiteral => Integer,
    StringLiteral => String, RawStringLiteral => String,
    ExprList => List, ExprArray => Array, ExprAngleBar => AngleBarList, ExprSequence => Sequence,
    ExprFunction => FunctionClosure, ExprQuote => Symbol, ExprOption => Option,
    ExprWhile => Nothing, Empty => Nothing, Stmt => Nothing,

}

/// Delegates one generated category to the rules of its existing alternatives.
macro_rules! delegate_variants {
    ($syntax:ty { $($variant:ident),+ $(,)? }) => {
        impl InferType for $syntax {
            fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
                match self { $(Self::$variant(value) => value.infer(context)),+ }
            }
        }
    };
}
delegate_variants!(Literal {
    Float,
    Integer,
    RawString,
    String
});

impl InferType for Expr {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        match self {
            Self::Literal(value) => value.infer(context),
            Self::Array(value) => value.infer(context),
            Self::List(value) => value.infer(context),
            Self::Sequence(value) => value.infer(context),
            Self::AngleBar(value) => value.infer(context),
            Self::Paren(value) => value.infer(context),
            Self::Binary(value) => value.infer(context),
            Self::Field(value) => context.field(value),
            Self::Prefix(value) => value.infer(context),
            Self::Postfix(value) => value.infer(context),
            Self::Assign(value) => value.infer(context),
            Self::Option(value) => value.infer(context),
            Self::Function(value) => value.infer(context),
            Self::If(value) => value.infer(context),
            Self::For(value) => value.infer(context),
            Self::While(value) => value.infer(context),
            Self::Try(value) => value.infer(context),
            Self::Quote(value) => value.infer(context),
            Self::New(value) => value.infer(context),
            Self::Break(value) => value.expr.as_deref().map_or_else(
                || context.exact(value, TypeRole::Nothing),
                |expr| context.expression(expr),
            ),
            Self::Continue(value) => value.expr.as_deref().map_or_else(
                || context.exact(value, TypeRole::Nothing),
                |expr| context.expression(expr),
            ),
            Self::Return(value) => value.expr.as_deref().map_or_else(
                || context.exact(value, TypeRole::Nothing),
                |expr| context.expression(expr),
            ),
            Self::Debug(value) => value.infer(context),
            Self::Timing(value) => value.infer(context),
            Self::Symbol(value) => value.infer(context),
            Self::Catch(_) | Self::Throw(_) | Self::Trap(_) => InferredType::unknown(),
        }
    }
}

impl InferType for Elements {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        match self {
            Self::Single(expression) => context.expression(expression),
            Self::Pack(_) => context.exact(self, TypeRole::Sequence),
        }
    }
}

impl InferType for ExprParen {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        self.expr.as_deref().map_or_else(
            || context.exact(self, TypeRole::Nothing),
            |value| context.expression(value),
        )
    }
}

impl InferType for Symbol {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.symbol(self)
    }
}

impl InferType for ExprAssign {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.expression(syntax::AssignmentValue::assigned_value(self))
    }
}

impl InferType for ExprIf {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let yes = context.expression(&self.then_clause.expr);
        let no = self.else_clause.as_ref().map_or_else(
            || context.exact(self, TypeRole::Nothing),
            |clause| context.expression(&clause.expr),
        );
        context.join(self, yes, no)
    }
}

impl InferType for ExprTry {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let success = context.expression(
            self.then_clause
                .as_ref()
                .map_or(&self.expr, |clause| &clause.expr),
        );
        // An exception-binding fallback remains conservative, as before: its
        // handler is checked independently rather than assumed to complete.
        let failure = match &self.fallback {
            Some(TryFallback::Else(clause)) => context.expression(&clause.expr),
            _ => context.exact(self, TypeRole::Nothing),
        };
        context.join(self, success, failure)
    }
}

impl InferType for ExprFor {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.exact(
            self,
            if self.body.list_clause.is_some() {
                TypeRole::List
            } else {
                TypeRole::Nothing
            },
        )
    }
}

impl InferType for ExprNew {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        match self.class.as_ref() {
            Expr::Symbol(name) => context.named_class(self, name.text()),
            _ => InferredType::unknown(),
        }
    }
}

impl InferType for ExprDebug {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        self.expr
            .as_deref()
            .map_or_else(InferredType::unknown, |value| context.expression(value))
    }
}

impl InferType for ExprTiming {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.expression(&self.expr)
    }
}

impl InferType for ExprBinary {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        if matches!(self.op, BinaryOperator::Space(_)) {
            context.application(self)
        } else {
            context.binary(self, self.op.spelling(), &self.left, &self.right)
        }
    }
}

impl InferType for ExprPrefix {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.unary(self, self.op.spelling(), &self.expr, OperatorForm::Prefix)
    }
}

impl InferType for ExprPostfix {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.unary(self, self.op.spelling(), &self.expr, OperatorForm::Postfix)
    }
}
