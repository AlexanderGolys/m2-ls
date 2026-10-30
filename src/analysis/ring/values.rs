//! Ring values and specialized operator inference over borrowed generated expressions.

use crate::node_metadata::SyntaxSnapshot;
use m2_syn::{nodes::*, Spanned};
use std::cell::OnceCell;

use super::syntax::RingSyntax;
use super::*;
use crate::analysis::inference::syntax::ExpressionSyntax;

/// Ring queries sharing one typed inference session and its expression lookup.
pub struct RingValues<'ast, 'query, S: ?Sized, K: ?Sized, F> {
    /// Source bindings and positioned type knowledge for this query.
    checker: &'query TypeChecker<'query, 'query, K>,
    /// Revision-aware mapping from typed syntax to source coordinates.
    source: &'query S,
    /// Borrowed expressions indexed by their semantic binding value ranges.
    expressions: &'query OnceCell<HashMap<SourceRangeKey, &'ast Expr>>,
    /// Retained AST cells supplying typed binding values on demand.
    snapshot: &'ast SyntaxSnapshot,
    /// Shared cached expression inference, retaining the requested lexical scope.
    infer: F,
    /// Scope inherited by operator operands in this query.
    scope: usize,
}

impl<'ast, 'query, S, K, F> RingValues<'ast, 'query, S, K, F>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
    F: Fn(&'ast Expr, usize) -> InferredType,
{
    /// Borrows a session's semantic context without rebuilding navigation indexes.
    pub fn new(
        checker: &'query TypeChecker<'query, 'query, K>,
        source: &'query S,
        snapshot: &'ast SyntaxSnapshot,
        expressions: &'query OnceCell<HashMap<SourceRangeKey, &'ast Expr>>,
        scope: usize,
        infer: F,
    ) -> Self {
        Self {
            checker,
            source,
            expressions,
            snapshot,
            infer,
            scope,
        }
    }

    /// Current source position of retained typed syntax.
    fn position(&self, syntax: &dyn Spanned) -> Option<Position> {
        Some(self.source.range_for_syntax(syntax)?.start)
    }

    /// Binding visible at this exact syntax position and its enclosing scope.
    fn binding(&self, symbol: &Symbol) -> Option<BindingView<'_>> {
        let position = self.position(symbol)?;
        self.checker.get_binding_from_scope(
            symbol.text(),
            self.checker.find_scope_at(position).unwrap_or(0),
            position,
        )
    }

    /// Whether a symbol is unbound and evaluates to its own symbolic name.
    fn bare_symbol(&self, symbol: &Symbol) -> bool {
        if self.binding(symbol).is_some() {
            return false;
        }
        let Some(range) = self.source.range_for_syntax(symbol) else {
            return false;
        };
        let Some(expression) = self.expression_at(range) else {
            return false;
        };
        let scope = self.checker.find_scope_at(range.start).unwrap_or(0);
        (self.infer)(expression, scope)
            .single()
            .is_some_and(|class| *class == TypeRole::Symbol.object_name())
    }

    /// Ring value retained on the source binding named by this expression.
    fn written(&self, expression: &Expr) -> Option<&RingValue> {
        let Expr::Symbol(symbol) = expression.unparenthesized()? else {
            return None;
        };
        match &self.binding(symbol)?.state.known_value {
            Some(KnownValue::Ring(value)) => Some(value),
            _ => None,
        }
    }

    /// Variable selected by a typed subscript, or its statically provable error.
    pub fn selected(
        &self,
        expression: &'ast Expr,
    ) -> Option<Result<SelectedRingVariable<'ast>, RingVariableError>> {
        let (left, right) = RingSyntax::operands(expression, "_")?;
        self.subscript(left, right)
    }

    /// Selection by a ring's literal position or by a target ring's variable name.
    fn subscript(
        &self,
        left: &'ast Expr,
        right: &'ast Expr,
    ) -> Option<Result<SelectedRingVariable<'ast>, RingVariableError>> {
        if let Some(ring) = self.written(left) {
            let IndexKey::Integer(index) = IndexKey::from_expression(right, &|_| false)? else {
                return None;
            };
            return Some(
                ring.subscript(index)?
                    .map(|variable| SelectedRingVariable {
                        ring: left,
                        variable,
                    })
                    .map_err(RingVariableError::IndexOutOfBounds),
            );
        }
        let ring = self.written(right)?;
        let (name, lookup) = self.variable_name(left)?;
        match ring.search(&name) {
            VariableSearch::Found(variable) => Some(Ok(SelectedRingVariable {
                ring: right,
                variable,
            })),
            VariableSearch::Absent => lookup
                .failure()
                .map(|failure| Err(RingVariableError::NotFound { name, failure })),
            VariableSearch::Undetermined => None,
        }
    }

    /// Printed variable name and the lookup method selected by its source value.
    fn variable_name(&self, expression: &'ast Expr) -> Option<(VariableName, VariableLookup)> {
        let expression = expression.unparenthesized()?;
        if let Some(text) = RingSyntax::string(expression) {
            return Some((VariableName(text), VariableLookup::String));
        }
        if let Expr::Symbol(symbol) = expression {
            let lookup = match self.binding(symbol) {
                None => VariableLookup::Symbol,
                Some(binding) if binding.state.indexed_element_type.is_some() => return None,
                Some(binding) if binding.state.is_ring_generator => VariableLookup::RingElement,
                Some(_) => return None,
            };
            return Some((VariableName(symbol.text().to_owned()), lookup));
        }
        if let Some(Ok(selected)) = self.selected(expression) {
            return Some((
                VariableName(selected.variable.to_string()),
                VariableLookup::RingElement,
            ));
        }
        let (left, right) = RingSyntax::operands(expression, "_")?;
        let index = IndexKey::from_expression(right, &|symbol| self.bare_symbol(symbol))?;
        let (base, lookup) = self.indexed_base(left, &index)?;
        Some((
            VariableName(format!("{base}_{}", index.subscript())),
            lookup,
        ))
    }

    /// Name underlying an indexed variable, respecting table writes and rebinding.
    fn indexed_base(
        &self,
        base: &'ast Expr,
        index: &IndexKey,
    ) -> Option<(VariableName, VariableLookup)> {
        let base = base.unparenthesized()?;
        if let Expr::Symbol(symbol) = base {
            let lookup = match self.binding(symbol) {
                None => VariableLookup::IndexedVariable,
                Some(binding) => match &binding.state.indexed_element_type {
                    Some(table) => match table.write_at(index) {
                        None => VariableLookup::IndexedVariable,
                        Some(write) if write.writer == IndexedWriter::RingConstructor => {
                            VariableLookup::RingElement
                        }
                        Some(_) => return None,
                    },
                    None if binding.state.is_ring_generator => VariableLookup::IndexedVariable,
                    None => return None,
                },
            };
            return Some((VariableName(symbol.text().to_owned()), lookup));
        }
        match self.selected(base)? {
            Ok(selected) if selected.variable.index.is_none() => Some((
                VariableName(selected.variable.base.name().to_owned()),
                VariableLookup::IndexedVariable,
            )),
            _ => None,
        }
    }

    /// Whether subscripting an unindexed generator produces an indexed variable.
    fn indexes_variable(&self, left: &'ast Expr, right: &'ast Expr) -> bool {
        IndexKey::from_expression(right, &|symbol| self.bare_symbol(symbol)).is_some()
            && match left.unparenthesized() {
                Some(Expr::Symbol(symbol)) => self.binding(symbol).is_some_and(|binding| {
                    binding.state.is_ring_generator && binding.state.indexed_element_type.is_none()
                }),
                _ => {
                    matches!(self.selected(left), Some(Ok(selected)) if selected.variable.index.is_none())
                }
            }
    }

    /// A polynomial constructor's typed variable specification, checked against its head type.
    fn constructor_variables(&self, application: &'ast ExprOpBinAdj) -> Option<&'ast Expr> {
        let variables = RingSyntax::constructor_variables(application)?;
        let position = self.position(application)?;
        let knowledge = self.checker.knowledge_at(position);
        (self.infer)(&application.left_operand, self.scope)
            .single()
            .is_some_and(|class| {
                knowledge.has_type_role(class, TypeRole::Ring)
                    || knowledge.is_subtype(class, &ObjectName::new(INEXACT_FIELD_FAMILY))
            })
            .then_some(variables)
    }

    /// Known constructor, quotient, forwarded ring, or basic ring value.
    pub fn value(&self, expression: &'ast Expr) -> Option<RingValue> {
        let expression = expression.unparenthesized()?;
        if let Expr::Op(ExprOp::Bin(ExprOpBin::Adj(application))) = expression {
            if let Some(variables) = self.constructor_variables(application) {
                return Some(RingValue::Polynomial {
                    coefficient_ring: self.value(&application.left_operand).map(Box::new),
                    variables: RingSyntax::generators(variables, &|symbol| {
                        self.bare_symbol(symbol)
                    })
                    .into_iter()
                    .map(|spec| spec.generator)
                    .collect(),
                });
            }
        }
        if let Some((left, _)) = RingSyntax::operands(expression, "/") {
            return self
                .value(left)
                .map(|ring| RingValue::Quotient(Box::new(ring)));
        }
        self.written(expression)
            .cloned()
            .or_else(|| self.basic(expression).then_some(RingValue::Basic))
    }

    /// All admitted classes are rings that cannot contribute polynomial variables.
    fn basic(&self, expression: &'ast Expr) -> bool {
        let Some(position) = self.position(expression) else {
            return false;
        };
        let knowledge = self.checker.knowledge_at(position);
        let inferred = (self.infer)(expression, self.scope);
        let variable_classes = VARIABLE_RING_CLASSES.map(ObjectName::new);
        let is_ring = |class: &ObjectName| {
            knowledge.has_type_role(class, TypeRole::Ring)
                || knowledge.is_subtype(class, &ObjectName::new(INEXACT_FIELD_FAMILY))
        };
        let mut exact = inferred.exact_points().peekable();
        let mut bounds = inferred.upward_generators().peekable();
        (exact.peek().is_some() || bounds.peek().is_some())
            && exact.all(|class| {
                is_ring(class)
                    && !variable_classes
                        .iter()
                        .any(|variable| knowledge.is_subtype(class, variable))
            })
            && bounds.all(|bound| {
                is_ring(bound)
                    && !variable_classes.iter().any(|variable| {
                        knowledge.is_subtype(bound, variable)
                            || knowledge.is_subtype(variable, bound)
                    })
            })
    }

    /// Ring, precision-field, indexed-variable, currying, and composition operator rules.
    pub fn operator(
        &self,
        operator: &str,
        left: &'ast Expr,
        right: &'ast Expr,
    ) -> Option<InferredType> {
        if !matches!(operator, "_" | "@@" | "/") {
            return None;
        }
        let position = self.position(left)?;
        let knowledge = self.checker.knowledge_at(position);
        if operator == "_" {
            if let Some(Ok(selected)) = self.subscript(left, right) {
                return self
                    .element_type(selected.ring, self.scope, &mut HashSet::new())
                    .map(InferredType::exact_from_id);
            }
            if self.indexes_variable(left, right) {
                return Some(InferredType::exact(INDEXED_VARIABLE));
            }
        }
        let left_type = (self.infer)(left, self.scope);
        let left_name = left_type.single()?;
        if operator == "_" && left_name == &TypeRole::Symbol.object_name() {
            return Some(InferredType::exact(INDEXED_VARIABLE));
        }
        if operator == "_"
            && knowledge.is_subtype(left_name, &ObjectName::new(INEXACT_FIELD_FAMILY))
        {
            let family = left.unparenthesized()?.symbol_name();
            let field = PRECISION_FIELD_CLASSES
                .iter()
                .find(|(name, _)| family == Some(*name))
                .map_or(INEXACT_FIELD, |(_, field)| field);
            return Some(InferredType::exact(field));
        }
        if operator == "_"
            && matches!(
                left.unparenthesized()?,
                Expr::Literal(ExprLiteral::Integer(_) | ExprLiteral::Float(_))
            )
        {
            let ring = right.unparenthesized()?;
            if knowledge.has_type_role((self.infer)(ring, self.scope).single()?, TypeRole::Ring) {
                let element = self
                    .element_type(ring, self.scope, &mut HashSet::new())
                    .unwrap_or_else(|| ObjectName::new(ANONYMOUS_RING_ELEMENT));
                return Some(InferredType::exact_from_id(element));
            }
        }
        if operator == "_" && left_name.as_ref() == INDEXED_VARIABLE_TABLE {
            if let Some(elements) = left
                .symbol_name()
                .and_then(|name| {
                    self.checker
                        .get_binding_from_scope(name, self.scope, position)
                })
                .and_then(|binding| binding.state.indexed_element_type.as_ref())
            {
                if let Some(index) = IndexKey::from_expression(right, &|_| false) {
                    return Some(elements.read_type(&index));
                }
                if self.written(right).is_some() {
                    return Some(elements.read_nonliteral_type());
                }
                return Some(
                    elements.writes.iter().fold(
                        InferredType::exact(INDEXED_VARIABLE),
                        |types, write| {
                            types | InferredType::exact_from_id(write.value_type.clone())
                        },
                    ),
                );
            }
            return Some(InferredType::exact(INDEXED_VARIABLE));
        }
        if operator == "/"
            && knowledge.has_type_role(left_name, TypeRole::Ring)
            && (self.infer)(right, self.scope).single()?.as_ref() == "ZZ"
        {
            return Some(InferredType::exact("QuotientRing"));
        }
        if matches!(operator, "_" | "@@") && knowledge.has_type_role(left_name, TypeRole::Function)
        {
            return Some(InferredType::exact("FunctionClosure"));
        }
        None
    }

    /// Resolve ring element identity through typed binding values, guarding alias cycles.
    fn element_type(
        &self,
        ring: &'ast Expr,
        scope: usize,
        visited: &mut HashSet<BindingId>,
    ) -> Option<ObjectName> {
        let ring = ring.unparenthesized()?;
        let name = ring.symbol_name()?;
        let position = self.position(ring)?;
        let knowledge = self.checker.knowledge_at(position);
        let Some(binding) = self
            .checker
            .visible_source_binding_from_scope(name, scope, position, &knowledge)
        else {
            let name = ObjectName::new(name);
            self.checker
                .resolve_type_id_at(&name, position, &knowledge)?;
            return Some(name);
        };
        if !visited.insert(binding.binding_id) {
            return None;
        }
        let value = self.expression_at(binding.state.value_range?)?;
        let value_scope = self
            .position(value)
            .and_then(|position| self.checker.find_scope_at(position))
            .unwrap_or(binding.state.scope_idx);
        if value.unparenthesized()?.symbol_name().is_some() {
            return self.element_type(value, value_scope, visited);
        }
        (binding.scope_idx == 0
            && binding.state.source_type.is_some()
            && self.introduces_identity(value, value_scope))
        .then(|| binding.name.clone())
    }

    /// Resolve a semantic binding value through retained typed expressions.
    fn expression_at(&self, range: TextRange) -> Option<&'ast Expr> {
        let expressions = self.expressions.get_or_init(|| {
            let mut visitor = ExpressionRanges {
                source: self.source,
                expressions: HashMap::new(),
            };
            for cell in self.snapshot.cells() {
                if let Some(cell) = cell.syntax() {
                    m2_syn::visit::Visit::visit_cell(&mut visitor, cell);
                }
            }
            visitor.expressions
        });
        expressions.get(&source_range_key(range)).copied()
    }

    /// Typed constructors create identities; references to existing rings preserve theirs.
    fn introduces_identity(&self, expression: &'ast Expr, scope: usize) -> bool {
        let Some(expression) = expression.unparenthesized() else {
            return false;
        };
        if matches!(expression, Expr::New(_)) {
            return true;
        }
        let operand = match expression {
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(application))) => {
                Some(application.left_operand.as_ref())
            }
            _ => RingSyntax::operands(expression, "/").map(|(left, _)| left),
        };
        operand.is_some_and(|operand| {
            let Some(position) = self.position(operand) else {
                return false;
            };
            let knowledge = self.checker.knowledge_at(position);
            (self.infer)(operand, scope).single().is_some_and(|class| {
                knowledge.has_type_role(class, TypeRole::Ring)
                    || knowledge.is_subtype(class, &ObjectName::new(INEXACT_FIELD_FAMILY))
            })
        })
    }
}

/// Typed expressions retained by the same revision, indexed for binding-value lookup.
struct ExpressionRanges<'ast, 'source, S: ?Sized> {
    /// Revision-aware mapping from generated spans to semantic source ranges.
    source: &'source S,
    /// Parent expression chosen for each exact binding-value range.
    expressions: HashMap<SourceRangeKey, &'ast Expr>,
}

impl<'ast, S: SourceNavigation + ?Sized> m2_syn::visit::Visit<'ast>
    for ExpressionRanges<'ast, '_, S>
{
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Some(range) = self.source.range_for_syntax(expression) {
            self.expressions
                .entry(source_range_key(range))
                .or_insert(expression);
        }
        m2_syn::visit::visit_expr(self, expression);
    }
}
