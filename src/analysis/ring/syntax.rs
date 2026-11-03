//! Generated syntax used by ring values and generator specifications.

use m2_syn::{nodes::*, Spanned, Token};

use super::{IndexKey, IndexSelection, RingGenerator, RingGeneratorKind};
use crate::analysis::inference::syntax::InfixSyntax;
use crate::analysis::syntax_roles::QuotedName;
use crate::object_registry::ObjectName;
use crate::syntax::{ExpressionSyntax, PackedArguments};

/// Typed source anchor of a generator declaration.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum GeneratorAnchor<'ast> {
    /// A direct or quoted symbol naming a generator.
    Symbol(&'ast Symbol),
    /// A range, string, or option value introducing generators.
    Expression(&'ast Expr),
}

impl<'ast> GeneratorAnchor<'ast> {
    /// The anchoring syntax, whose span locates the declaration.
    pub fn syntax(self) -> &'ast dyn Spanned {
        match self {
            Self::Symbol(symbol) => symbol,
            Self::Expression(expression) => expression,
        }
    }
}

/// A generator specification together with its exact typed source anchor.
#[derive(Debug, Clone)]
pub struct GeneratorSpec<'ast> {
    /// Semantic generator identity and compact index selection.
    pub generator: RingGenerator,
    /// Syntax responsible for introducing this generator.
    pub anchor: GeneratorAnchor<'ast>,
}

/// Structural projections of generated expressions used by ring semantics.
pub struct RingSyntax;

impl RingSyntax {
    /// Operands of a requested infix operator, with parentheses peeled.
    pub fn operands<'ast>(
        expression: &'ast Expr,
        operator: &str,
    ) -> Option<(&'ast Expr, &'ast Expr)> {
        let parts = expression.unparenthesized()?.infix_operands()?;
        (parts.operator == operator).then_some((parts.left, parts.right))
    }

    /// Immediate collection elements, excluding empty slots and muted statements.
    pub fn elements(expression: &Expr) -> Option<Vec<&Expr>> {
        let contents = match expression.unparenthesized()? {
            Expr::Collection(ExprCollection::Array(array)) => &array.elements,
            Expr::Collection(ExprCollection::List(list)) => &list.elements,
            Expr::Collection(ExprCollection::Sequence(sequence)) => &sequence.elements,
            _ => return None,
        };
        Some(
            contents
                .as_ref()
                .map_or_else(Vec::new, PackedArguments::arguments)
                .into_iter()
                .flatten()
                .collect(),
        )
    }

    /// Literal contents whose spelling can be compared without evaluating escapes.
    pub fn string(expression: &Expr) -> Option<String> {
        Self::literal_contents(expression).filter(|text| !text.contains('\\'))
    }

    /// Original literal contents from generated elements, preserving escape spelling.
    pub fn literal_contents(expression: &Expr) -> Option<String> {
        match expression.unparenthesized()? {
            Expr::Literal(ExprLiteral::String(string)) => string
                .elements
                .iter()
                .map(|element| Some(element.try_span()?.text()?.to_owned()))
                .collect(),
            Expr::Literal(ExprLiteral::RawString(string)) => string
                .elements
                .iter()
                .map(|element| Some(element.try_span()?.text()?.to_owned()))
                .collect(),
            _ => None,
        }
    }

    /// Symbol named directly, through parentheses, or by a quote expression.
    pub fn name(expression: &Expr) -> Option<&Symbol> {
        match expression.unparenthesized()? {
            Expr::Symbol(symbol) => Some(symbol),
            Expr::Quote(quote) => Some(quote.quoted_name()),
            _ => None,
        }
    }

    /// Value of an immediate named option.
    pub fn option<'ast>(expression: &'ast Expr, key: &str) -> Option<&'ast Expr> {
        match expression {
            Expr::Option(option) if option.key.symbol_name() == Some(key) => Some(&option.value),
            _ => None,
        }
    }

    /// The delimited variable specification of a polynomial constructor.
    pub fn constructor_variables(application: &ExprOpBinAdj) -> Option<&Expr> {
        let argument = application.right_operand.unparenthesized()?;
        let collection = |expression: &Expr| {
            matches!(
                expression,
                Expr::Collection(ExprCollection::Array(_) | ExprCollection::List(_))
            )
        };
        if collection(argument) {
            return Some(argument);
        }
        let left = argument.infix_operands()?.left;
        collection(left).then_some(left)
    }

    /// A constructor's ordered generator specifications, retaining compact ranges.
    pub fn generators<'ast>(
        container: &'ast Expr,
        bare: &dyn Fn(&Symbol) -> bool,
    ) -> Vec<GeneratorSpec<'ast>> {
        let elements = Self::elements(container).unwrap_or_default();
        let base = elements
            .iter()
            .find_map(|element| Self::option(element, "VariableBaseName"))
            .and_then(|expression| {
                expression
                    .symbol_name()
                    .map(str::to_owned)
                    .or_else(|| Self::literal_contents(expression))
            });
        let mut specs = Vec::new();
        for element in elements {
            if matches!(element, Expr::Option(_)) {
                if let Some(variables) = Self::option(element, "Variables") {
                    if let Expr::Literal(ExprLiteral::Integer(integer)) = variables {
                        let indices = integer
                            .text()
                            .parse::<i64>()
                            .ok()
                            .filter(|count| *count > 0)
                            .map(|count| IndexSelection::Range {
                                start: IndexKey::Integer(0),
                                end: IndexKey::Integer(count - 1),
                                exclusive: false,
                            });
                        Self::push(
                            &mut specs,
                            GeneratorSpec {
                                generator: RingGenerator {
                                    name: ObjectName::new(
                                        base.clone().unwrap_or_else(|| "p".to_owned()),
                                    ),
                                    kind: RingGeneratorKind::IndexedTable,
                                    indices,
                                },
                                anchor: GeneratorAnchor::Expression(variables),
                            },
                        );
                    } else {
                        Self::collect(variables, &mut specs, bare);
                    }
                }
            } else {
                Self::collect(element, &mut specs, bare);
            }
        }
        specs
    }

    /// Generator declarations contributed by a single typed specification.
    pub fn specifications<'ast>(
        expression: &'ast Expr,
        bare: &dyn Fn(&Symbol) -> bool,
    ) -> Vec<GeneratorSpec<'ast>> {
        let mut specs = Vec::new();
        Self::collect(expression, &mut specs, bare);
        specs
    }

    /// Collects one typed specification, recursively following collection fields.
    fn collect<'ast>(
        expression: &'ast Expr,
        specs: &mut Vec<GeneratorSpec<'ast>>,
        bare: &dyn Fn(&Symbol) -> bool,
    ) {
        let Some(expression) = expression.unparenthesized() else {
            return;
        };
        if let Expr::Op(ExprOp::Bin(ExprOpBin::Adj(application))) = expression {
            if application.left_operand.symbol_name() == Some(super::GET_SYMBOL_FUNCTION) {
                if let Some(name) = Self::literal_contents(&application.right_operand) {
                    Self::push(
                        specs,
                        GeneratorSpec {
                            generator: RingGenerator {
                                name: ObjectName::new(name),
                                kind: RingGeneratorKind::Direct,
                                indices: None,
                            },
                            anchor: GeneratorAnchor::Expression(
                                application
                                    .right_operand
                                    .unparenthesized()
                                    .unwrap_or(&application.right_operand),
                            ),
                        },
                    );
                }
                return;
            }
        }
        if let Some(symbol) = Self::name(expression) {
            Self::push(
                specs,
                GeneratorSpec {
                    generator: RingGenerator {
                        name: ObjectName::new(symbol.text()),
                        kind: RingGeneratorKind::Direct,
                        indices: None,
                    },
                    anchor: GeneratorAnchor::Symbol(symbol),
                },
            );
            return;
        }
        if let Some((base, index)) = Self::operands(expression, "_") {
            if let Some(symbol) = Self::name(base) {
                Self::push(
                    specs,
                    GeneratorSpec {
                        generator: RingGenerator {
                            name: ObjectName::new(symbol.text()),
                            kind: RingGeneratorKind::IndexedTable,
                            indices: IndexKey::from_expression(index, bare)
                                .map(IndexSelection::Exact),
                        },
                        anchor: GeneratorAnchor::Symbol(symbol),
                    },
                );
            }
            return;
        }
        if let Some(parts) = expression
            .infix_operands()
            .filter(|parts| matches!(parts.operator, ".." | "..<"))
        {
            let indexed = Self::operands(parts.left, "_").zip(Self::operands(parts.right, "_"));
            let (left, right, kind, indices) = if let Some(((left, start), (right, end))) = indexed
            {
                let indices = IndexKey::from_expression(start, bare)
                    .zip(IndexKey::from_expression(end, bare))
                    .map(|(start, end)| IndexSelection::Range {
                        start,
                        end,
                        exclusive: parts.operator == "..<",
                    });
                (
                    Self::name(left),
                    Self::name(right),
                    RingGeneratorKind::IndexedTable,
                    indices,
                )
            } else {
                (
                    Self::name(parts.left),
                    Self::name(parts.right),
                    RingGeneratorKind::Direct,
                    None,
                )
            };
            if let Some((left, right)) = left.zip(right) {
                if let Some(names) =
                    Self::symbol_range(left.text(), right.text(), parts.operator == "..<")
                {
                    for name in names {
                        Self::push(
                            specs,
                            GeneratorSpec {
                                generator: RingGenerator {
                                    name: ObjectName::new(name),
                                    kind,
                                    indices: indices.clone(),
                                },
                                anchor: GeneratorAnchor::Expression(expression),
                            },
                        );
                    }
                    return;
                }
            }
        }
        if let Some(elements) = Self::elements(expression) {
            for element in elements {
                Self::collect(element, specs, bare);
            }
        }
    }

    /// Names in Macaulay2's ordered alphabet, including equal multi-letter endpoints.
    fn symbol_range(left: &str, right: &str, exclusive: bool) -> Option<Vec<String>> {
        if left == right {
            return Some(if exclusive {
                Vec::new()
            } else {
                vec![left.to_owned()]
            });
        }
        let names = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let start = names
            .chars()
            .position(|letter| letter.to_string() == left)?;
        let end = names
            .chars()
            .position(|letter| letter.to_string() == right)?
            + usize::from(!exclusive);
        Some(
            names
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .map(|letter| letter.to_string())
                .collect(),
        )
    }

    /// Retains the first occurrence of an equivalent generator specification.
    fn push<'ast>(specs: &mut Vec<GeneratorSpec<'ast>>, spec: GeneratorSpec<'ast>) {
        if !specs
            .iter()
            .any(|existing| existing.generator == spec.generator)
        {
            specs.push(spec);
        }
    }
}

impl IndexKey {
    /// Literal or symbolic selector read directly from generated operand fields.
    pub fn from_expression(expression: &Expr, bare: &dyn Fn(&Symbol) -> bool) -> Option<Self> {
        let expression = expression.unparenthesized()?;
        match expression {
            Expr::Literal(ExprLiteral::Integer(integer)) => {
                return integer.text().parse().ok().map(Self::Integer)
            }
            Expr::Symbol(symbol) => {
                return bare(symbol).then(|| Self::Symbol(ObjectName::new(symbol.text())))
            }
            Expr::Op(ExprOp::Prefix(prefix))
                if prefix.operator.spelling() == <Token![-]>::SPELLING =>
            {
                let Self::Integer(value) = Self::from_expression(&prefix.operand, bare)? else {
                    return None;
                };
                return value.checked_neg().map(Self::Integer);
            }
            _ => {}
        }
        if let Some((base, index)) = RingSyntax::operands(expression, "_") {
            let Expr::Symbol(symbol) = base.unparenthesized()? else {
                return None;
            };
            return bare(symbol)
                .then(|| Self::from_expression(index, bare))
                .flatten()
                .map(|index| Self::IndexedVariable {
                    base: ObjectName::new(symbol.text()),
                    index: Box::new(index),
                });
        }
        if let Some(text) = RingSyntax::string(expression) {
            return Some(Self::String(text));
        }
        let items = RingSyntax::elements(expression)?
            .into_iter()
            .map(|element| Self::from_expression(element, bare))
            .collect::<Option<Vec<_>>>()?;
        match expression {
            Expr::Collection(ExprCollection::List(_)) => Some(Self::List(items)),
            Expr::Collection(ExprCollection::Sequence(_)) => Some(Self::Sequence(items)),
            _ => None,
        }
    }
}
