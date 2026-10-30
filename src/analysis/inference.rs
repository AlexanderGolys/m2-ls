//! Type rules on generated syntax, independent of document navigation.
//!
//! Each concrete construct owns its inference rule. Generated enums only
//! delegate to their alternatives; operands remain borrowed typed expressions.
//! The context supplies positioned semantic knowledge and memoized recursion.

use m2_syn::{nodes::*, Spanned};

use super::syntax_roles::OperatorSyntax;
use crate::object_registry::OperatorForm;
use crate::typesystem::{InferredType, TypeRole};

pub mod navigation;
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
    fn application(&self, application: &'ast ExprOpBinAdj) -> InferredType;
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
    List => List, Array => Array, AngleBarList => AngleBarList, Sequence => Sequence,
    ExprLambda => FunctionClosure, ExprQuote => Symbol, ExprOption => Option,
    ExprWhile => Nothing, Empty => Nothing, Statement => Nothing, ExprPack => Sequence,
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
delegate_variants!(ExprLiteral {
    Float,
    Integer,
    RawString,
    String
});
delegate_variants!(ExprCollection {
    Sequence,
    List,
    Array,
    AngleBar
});
delegate_variants!(ExprOp {
    Bin,
    Prefix,
    Postfix
});
delegate_variants!(ExprOpBin {
    Adj,
    Regular,
    Member,
    Index
});
delegate_variants!(PackedContent { Expr, Pack });

impl InferType for Expr {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        match self {
            Self::Literal(value) => value.infer(context),
            Self::Collection(value) => value.infer(context),
            Self::Holder(value) => value.infer(context),
            Self::Op(value) => value.infer(context),
            Self::Assign(value) => value.infer(context),
            Self::Option(value) => value.infer(context),
            Self::Lambda(value) => value.infer(context),
            Self::If(value) => value.infer(context),
            Self::For(value) => value.infer(context),
            Self::While(value) => value.infer(context),
            Self::Try(value) => value.infer(context),
            Self::Quote(value) => value.infer(context),
            Self::New(value) => value.infer(context),
            Self::Control(value) => value.infer(context),
            Self::Debug(value) => value.infer(context),
            Self::Profile(value) => value.infer(context),
            Self::Symbol(value) => value.infer(context),
            Self::Catch(_) | Self::Throw(_) | Self::Trap(_) => InferredType::unknown(),
        }
    }
}

impl InferType for Holder {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        self.value.as_deref().map_or_else(
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
            self.on_success
                .as_ref()
                .map_or(&self.value, |clause| &clause.expr),
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
        let body = match self {
            Self::Bare(loop_) => &loop_.body,
            Self::Range(loop_) => &loop_.body,
            Self::In(loop_) => &loop_.body,
        };
        context.exact(
            self,
            if body.list_step.is_some() {
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

impl InferType for ExprControl {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let value = match self {
            Self::Break(transfer) => &transfer.value,
            Self::Continue(transfer) => &transfer.value,
            Self::Return(transfer) => &transfer.value,
        };
        value.as_deref().map_or_else(
            || context.exact(self, TypeRole::Nothing),
            |value| context.expression(value),
        )
    }
}

impl InferType for ExprDebug {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let value = match self {
            Self::Test(debug) => &debug.value,
            Self::Step(debug) => &debug.value,
            Self::Finish(debug) => &debug.value,
            Self::Shield(debug) => &debug.value,
            Self::Breakpoint(debug) => &debug.value,
        };
        value
            .as_deref()
            .map_or_else(InferredType::unknown, |value| context.expression(value))
    }
}

impl InferType for ExprProfile {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let value = match self {
            Self::ElapsedTime(profile) => &profile.value,
            Self::ElapsedTiming(profile) => &profile.value,
            Self::Profile(profile) => &profile.value,
            Self::Time(profile) => &profile.value,
            Self::Timing(profile) => &profile.value,
        };
        context.expression(value)
    }
}

impl InferType for ExprOpBinAdj {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        context.application(self)
    }
}

impl InferType for ExprOpBinRegular {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let spelling = self.operator.operator_spelling();
        context.binary(
            self,
            spelling.as_deref().unwrap_or_default(),
            &self.left_operand,
            &self.right_operand,
        )
    }
}

impl InferType for ExprOpBinIndex {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        use m2_syn::Token;
        context.binary(
            self,
            <m2_syn::Token![#]>::SPELLING,
            &self.receiver,
            &self.index,
        )
    }
}

impl InferType for ExprOpBinMember {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        use m2_syn::Token;
        match self {
            Self::Dot(access) => context.binary(
                self,
                <m2_syn::Token![.]>::SPELLING,
                &access.receiver,
                &access.member,
            ),
            Self::Optional(access) => context.binary(
                self,
                <m2_syn::Token![.?]>::SPELLING,
                &access.receiver,
                &access.member,
            ),
        }
    }
}

impl InferType for ExprOpPrefix {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let spelling = self.operator.operator_spelling();
        context.unary(
            self,
            spelling.as_deref().unwrap_or_default(),
            &self.operand,
            OperatorForm::Prefix,
        )
    }
}

impl InferType for ExprOpPostfix {
    fn infer<'ast>(&'ast self, context: &dyn InferenceContext<'ast>) -> InferredType {
        let spelling = self.operator.operator_spelling();
        context.unary(
            self,
            spelling.as_deref().unwrap_or_default(),
            &self.operand,
            OperatorForm::Postfix,
        )
    }
}
