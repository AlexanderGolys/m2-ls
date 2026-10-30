//! Positioned inference sessions with borrowed expression identity and scope-aware caching.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ptr;

use m2_syn::{nodes::*, Spanned};
use tower_lsp::lsp_types::Position;

use crate::analysis::{CallStaticFacts, Operator, OutputReference, TypeChecker};
use crate::node_metadata::SyntaxSnapshot;
use crate::object_registry::{ObjectKnowledge, ObjectName, OperatorForm};
use crate::source::SourceNavigation;
use crate::typesystem::{
    InferredType, LiteralOption, PositionedTypeKnowledge, TypeKnowledge, TypeRole,
};

use super::syntax::{ExpressionSyntax, InfixSyntax, MethodConstructor};
use super::{InferType, InferenceContext};
use crate::analysis::ring::values::RingValues;
use crate::analysis::SourceRangeKey;

/// Borrowed identity of one generated expression, independent of source-span equality.
#[derive(Debug, Clone, Copy)]
struct ExpressionIdentity<'ast>(&'ast Expr);
impl PartialEq for ExpressionIdentity<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}
impl Eq for ExpressionIdentity<'_> {}
impl Hash for ExpressionIdentity<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::from_ref(self.0).hash(state);
    }
}

/// One immutable syntax revision and knowledge view with a transient inference cache.
pub struct InferenceSession<'ast, 'request, S: ?Sized, K: ?Sized> {
    /// Semantic analysis and external knowledge used by all queries in this session.
    checker: &'request TypeChecker<'request, 'request, K>,
    /// Original document text and the revision's cell-offset mapping.
    source: &'request S,
    /// Retained cells supplying output-history values and AST ownership.
    snapshot: &'ast SyntaxSnapshot,
    /// Typed expression locations for retained binding values and symbolic selectors.
    expressions: OnceCell<HashMap<SourceRangeKey, &'ast Expr>>,
    /// Results for exact AST allocations in their requested lexical scopes.
    cache: RefCell<HashMap<(ExpressionIdentity<'ast>, usize), InferredType>>,
}

impl<'ast, 'request, S, K> InferenceSession<'ast, 'request, S, K>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
{
    /// Starts a cache whose borrowed keys cannot outlive this syntax revision.
    pub fn new(
        checker: &'request TypeChecker<'request, 'request, K>,
        source: &'request S,
        snapshot: &'ast SyntaxSnapshot,
    ) -> Self {
        Self {
            checker,
            source,
            snapshot,
            expressions: OnceCell::new(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Ring-value queries share this session's typed operands and inference cache.
    pub fn ring_values(
        &self,
        scope: usize,
    ) -> RingValues<'ast, '_, S, K, impl Fn(&'ast Expr, usize) -> InferredType + '_> {
        RingValues::new(
            self.checker,
            self.source,
            self.snapshot,
            &self.expressions,
            scope,
            |expression, scope| self.infer(expression, scope),
        )
    }

    /// Binds expression rules to one lexical scope without creating another cache.
    pub fn context(&self, scope: usize) -> ExpressionContext<'_, 'ast, 'request, S, K> {
        ExpressionContext {
            session: self,
            scope,
        }
    }

    /// Lexical scope containing typed syntax, defaulting to the document scope.
    pub fn scope_at(&self, syntax: &dyn Spanned) -> usize {
        self.source
            .range_for_syntax(syntax)
            .and_then(|range| self.checker.find_scope_at(range.start))
            .unwrap_or(0)
    }

    /// Infers an expression, distinguishing identical spans and different lexical scopes.
    pub fn infer(&self, expression: &'ast Expr, scope: usize) -> InferredType {
        let key = (ExpressionIdentity(expression), scope);
        if let Some(result) = self.cache.borrow().get(&key) {
            return result.clone();
        }
        let result = expression.infer(&self.context(scope));
        self.cache.borrow_mut().insert(key, result.clone());
        result
    }
}

/// Context inherited by typed operands while inferring one expression.
pub struct ExpressionContext<'session, 'ast, 'request, S: ?Sized, K: ?Sized> {
    /// Session whose cache and knowledge are shared by recursive operands.
    session: &'session InferenceSession<'ast, 'request, S, K>,
    /// Lexical scope requested by the caller; part of the memoization key.
    scope: usize,
}

impl<'ast, S, K> ExpressionContext<'_, 'ast, '_, S, K>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
{
    /// Source position of a typed construct in this revision, absent for foreign syntax.
    pub fn position(&self, syntax: &dyn Spanned) -> Option<Position> {
        Some(self.session.source.range_for_syntax(syntax)?.start)
    }

    /// The original spelling of a typed expression, retaining string delimiters.
    pub fn text(&self, syntax: &dyn Spanned) -> Option<&str> {
        self.session
            .source
            .text()
            .get(self.session.source.syntax_bytes(syntax)?)
    }

    /// Argument types and literal option values, preserving empty positional slots.
    pub fn call_facts(
        &self,
        argument: &'ast Expr,
        callable: Option<&str>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        let argument = argument.unparenthesized().unwrap_or(argument);
        let argument_owner = argument;
        let receives_sequence = callable.is_some_and(|name| {
            self.position(argument).is_some_and(|position| {
                self.session
                    .checker
                    .callable_receives_sequence(name, self.scope, position, knowledge)
            })
        });
        let arguments = if receives_sequence {
            vec![Some(argument)]
        } else {
            argument.arguments()
        };
        let mut facts = CallStaticFacts::default();
        for argument in arguments {
            if let Some(Expr::Option(option)) = argument {
                if let (Some(key), Some(value)) = (
                    option.key.symbol_name(),
                    self.literal_option_value(&option.value),
                ) {
                    facts.literal_options.push(LiteralOption {
                        option: ObjectName::new(key),
                        value: ObjectName::new(value),
                    });
                    continue;
                }
            }
            facts.argument_types.push(argument.map_or_else(
                || self.exact(argument_owner, TypeRole::Nothing),
                |value| self.expression(value),
            ));
        }
        facts
    }

    /// Original literal spelling accepted by the option dispatch table.
    pub fn literal_option_value(&self, expression: &Expr) -> Option<&str> {
        match expression {
            Expr::Symbol(_) | Expr::Literal(_) => self.text(expression),
            _ => None,
        }
    }

    /// Selects an earlier unmuted top-level value without navigating a synthetic tree.
    pub fn output_type(&self, symbol: &'ast Symbol, reference: OutputReference) -> InferredType {
        let Some(bytes) = self.session.source.syntax_bytes(symbol) else {
            return InferredType::unknown();
        };
        let cells = self.session.snapshot.cells();
        let Some(current) = cells
            .iter()
            .find(|cell| cell.document_range().contains(&bytes.start))
        else {
            return self.exact(symbol, TypeRole::Symbol);
        };
        if current.syntax().is_none_or(|cell| cell.semicolon.is_some()) {
            return self.exact(symbol, TypeRole::Symbol);
        }
        let previous = cells
            .iter()
            .take_while(|cell| cell.document_range().end <= current.document_range().start)
            .filter_map(|cell| cell.syntax())
            .filter(|cell| cell.semicolon.is_none())
            .collect::<Vec<_>>();
        let selected = match reference {
            OutputReference::Relative(distance) => {
                previous.iter().rev().nth(distance.get() - 1).copied()
            }
            OutputReference::Absolute(number) => previous.get(number.get() - 1).copied(),
            OutputReference::MissingAbsolute => None,
        };
        let Some(cell) = selected else {
            return self.exact(symbol, TypeRole::Symbol);
        };
        let scope = self
            .position(&cell.content)
            .and_then(|position| self.session.checker.find_scope_at(position))
            .unwrap_or(self.scope);
        cell.content.infer(&self.session.context(scope))
    }

    /// Infers application with an explicitly selected argument, including precedence lowering.
    pub fn application_argument(
        &self,
        application: &'ast ExprOpBinAdj,
        argument: &'ast Expr,
    ) -> InferredType {
        if let Some(result) = self.trailing_operator(application, argument) {
            return result;
        }
        let Some(position) = self.position(application) else {
            return InferredType::unknown();
        };
        let checker = self.session.checker;
        let knowledge = checker.knowledge_at(position);
        let callable = &application.left_operand;
        let name = callable.symbol_name();
        let facts = self.call_facts(argument, name, &knowledge);
        let Some(callable_position) = self.position(callable.as_ref()) else {
            return InferredType::unknown();
        };
        if let Some(name) = name {
            if let Some(function) =
                checker.local_function_at(name, self.scope, callable_position, &knowledge)
            {
                return checker
                    .resolve_local_call_return_type(
                        function,
                        &facts.argument_types,
                        callable_position,
                        &knowledge,
                    )
                    .unwrap_or_else(InferredType::unknown);
            }
            if name == "error"
                && checker
                    .visible_source_binding_from_scope(
                        name,
                        self.scope,
                        callable_position,
                        &knowledge,
                    )
                    .is_none()
                && knowledge.get_record(&ObjectName::new(name)).is_some()
            {
                return InferredType::diverges();
            }
        }
        let head = self.expression(callable);
        if head
            .single()
            .is_some_and(|class| knowledge.has_type_role(class, TypeRole::Function))
        {
            return name
                .and_then(|name| {
                    checker.resolve_external_call_return_type(
                        &ObjectName::new(name),
                        &facts.argument_types,
                        &facts.literal_options,
                        position,
                        &knowledge,
                    )
                })
                .unwrap_or_else(InferredType::unknown);
        }
        checker.dispatch_codomain(
            &knowledge,
            &Operator {
                token: ObjectName::new(
                    crate::macaulay2::Macaulay2Conventions::APPLICATION_OPERATOR,
                ),
                form: OperatorForm::Binary,
            },
            &[head, self.expression(argument)],
            &facts.literal_options,
            position,
        )
    }

    /// Applies delimited arguments before a trailing infix operator without reparsing source.
    pub fn trailing_operator(
        &self,
        application: &'ast ExprOpBinAdj,
        argument: &'ast Expr,
    ) -> Option<InferredType> {
        let binary = argument.infix_operands()?;
        let left = binary.left;
        let applied = if matches!(left, Expr::Collection(_) | Expr::Holder(_)) {
            self.application_argument(application, left)
        } else {
            self.trailing_operator(application, left)?
        };
        let position = self.position(argument)?;
        let knowledge = self.session.checker.knowledge_at(position);
        Some(self.session.checker.dispatch_codomain(
            &knowledge,
            &Operator {
                token: ObjectName::new(binary.operator),
                form: OperatorForm::Binary,
            },
            &[applied, self.expression(binary.right)],
            &[],
            position,
        ))
    }
}

impl<'ast, S, K> InferenceContext<'ast> for ExpressionContext<'_, 'ast, '_, S, K>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
{
    fn expression(&self, expression: &'ast Expr) -> InferredType {
        self.session.infer(expression, self.scope)
    }
    fn exact(&self, syntax: &dyn Spanned, class: TypeRole) -> InferredType {
        self.named_class(syntax, class.object_name().name())
    }
    fn named_class(&self, syntax: &dyn Spanned, name: &str) -> InferredType {
        let Some(position) = self.position(syntax) else {
            return InferredType::unknown();
        };
        let knowledge = self.session.checker.knowledge_at(position);
        let name = ObjectName::new(name);
        self.session
            .checker
            .resolve_type_at(&name, position, &knowledge)
            .map(InferredType::exact_type)
            .unwrap_or_else(|| InferredType::exact_from_id(name))
    }
    fn symbol(&self, symbol: &'ast Symbol) -> InferredType {
        let Some(position) = self.position(symbol) else {
            return InferredType::unknown();
        };
        let checker = self.session.checker;
        let knowledge = checker.knowledge_at(position);
        let name = symbol.text();
        if let Some(binding) =
            checker.visible_source_binding_from_scope(name, self.scope, position, &knowledge)
        {
            return binding
                .state
                .inferred_type
                .clone()
                .unwrap_or_else(InferredType::unknown);
        }
        if let Some(reference) = OutputReference::parse(name) {
            return self.output_type(symbol, reference);
        }
        knowledge.get_record(&ObjectName::new(name)).map_or_else(
            || self.exact(symbol, TypeRole::Symbol),
            |record| InferredType::exact_from_id(record.class.clone()),
        )
    }
    fn application(&self, application: &'ast ExprOpBinAdj) -> InferredType {
        if let Some(class) = application.method_class() {
            return self.exact(application, class);
        }
        self.application_argument(application, &application.right_operand)
    }
    fn binary(
        &self,
        syntax: &dyn Spanned,
        operator: &str,
        left: &'ast Expr,
        right: &'ast Expr,
    ) -> InferredType {
        let Some(position) = self.position(syntax) else {
            return InferredType::unknown();
        };
        let checker = self.session.checker;
        let knowledge = checker.knowledge_at(position);
        if let Some(result) = self
            .session
            .ring_values(self.scope)
            .operator(operator, left, right)
        {
            return result;
        }
        let operator = Operator {
            token: ObjectName::new(operator),
            form: OperatorForm::Binary,
        };
        let arguments = [self.expression(left), self.expression(right)];
        if operator.is_comparison() {
            checker
                .resolved_dispatch_codomain(&knowledge, &operator, &arguments, &[], position)
                .unwrap_or_else(|| self.exact(syntax, TypeRole::Boolean))
        } else {
            checker.dispatch_codomain(&knowledge, &operator, &arguments, &[], position)
        }
    }
    fn unary(
        &self,
        syntax: &dyn Spanned,
        operator: &str,
        operand: &'ast Expr,
        form: OperatorForm,
    ) -> InferredType {
        let Some(position) = self.position(syntax) else {
            return InferredType::unknown();
        };
        let checker = self.session.checker;
        let knowledge = checker.knowledge_at(position);
        checker.dispatch_codomain(
            &knowledge,
            &Operator {
                token: ObjectName::new(operator),
                form,
            },
            &[self.expression(operand)],
            &[],
            position,
        )
    }
    fn join(&self, syntax: &dyn Spanned, left: InferredType, right: InferredType) -> InferredType {
        let Some(position) = self.position(syntax) else {
            return InferredType::unknown();
        };
        let knowledge = self.session.checker.knowledge_at(position);
        self.session
            .checker
            .join_types(left, right, position, &knowledge)
    }
}

#[cfg(test)]
mod tests {
    //! Cache correctness when one borrowed expression is queried in distinct scopes.

    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;
    use m2_syn::visit::Visit;

    /// Finds a variadic lambda's typed body without using navigation casts.
    #[derive(Default)]
    struct LambdaBody<'ast> {
        /// Borrowed generated body collected by the visitor.
        expression: Option<&'ast Expr>,
    }
    impl<'ast> Visit<'ast> for LambdaBody<'ast> {
        fn visit_expr_lambda_variadic(&mut self, lambda: &'ast ExprLambdaVariadic) {
            self.expression = Some(&lambda.body);
        }
    }

    /// A cache entry in the document scope cannot supply the result for the same
    /// AST allocation under a lambda parameter that shadows the document binding.
    #[test]
    fn expression_cache_distinguishes_requested_lexical_scopes() {
        let registry = ObjectRegistry::default();
        let document = DocumentSnapshot::from_text(
            "value = \"outer\"\nreader = value -> value\n".into(),
            &registry,
        )
        .unwrap();
        let root = document.root_node();
        let mut body = LambdaBody::default();
        root.typed_source().visit(&mut body);
        let expression = body.expression.unwrap();
        let checker = TypeChecker::new(document.analysis(), &registry);
        let session = InferenceSession::new(&checker, &document, root.snapshot());
        let scope = checker
            .find_scope_at(document.range_for_syntax(expression).unwrap().start)
            .unwrap();
        assert_ne!(scope, 0);
        assert_eq!(
            session.infer(expression, 0).single().map(ObjectName::name),
            Some("String")
        );
        assert_eq!(session.infer(expression, scope), InferredType::unknown());
        assert_eq!(
            session.infer(expression, 0).single().map(ObjectName::name),
            Some("String")
        );
    }
}
