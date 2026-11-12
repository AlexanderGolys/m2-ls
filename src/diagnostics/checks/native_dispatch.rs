//! Operations the interpreter evaluates itself, before it looks up any
//! installed method.
//!
//! M2's evaluator answers some operator calls directly: when the operands are
//! native values (`ZZ`, `QQ`, `RR`, `RRi`, `CC`, `CCi`, Booleans, strings,
//! symbols, nets, `null`) it computes the result and only otherwise falls back
//! to method lookup. A method installed for exactly those operand classes is
//! recorded, and listed by `methods`, but never called. Nothing in the
//! method table tells the two apart, so the table here is the knowledge: it is
//! held against the real interpreter by `tests/native_dispatch`, whose probe
//! installs a marker method for every operand class pair of every operator.
//!
//! Ring elements of polynomial rings, matrices, ideals, lists and every other
//! class dispatch normally; so do `**`, `%`, `|`, `<<`, `_` and the rest of
//! the flexible operators. Only the operators below have native operand pairs.

use std::fmt::{Display, Formatter, Result};

use m2_syn::Token;
use strum::EnumString;

use crate::object_registry::ObjectName;

/// A class whose values the interpreter represents natively, so that it can
/// compute some operations on them without looking up a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
pub enum NativeClass {
    /// Machine-independent integers, `ZZ`.
    #[strum(serialize = "ZZ")]
    Integer,
    /// Rational numbers, `QQ`.
    #[strum(serialize = "QQ")]
    Rational,
    /// Floating-point reals, `RR`.
    #[strum(serialize = "RR")]
    Real,
    /// Real intervals, `RRi`.
    #[strum(serialize = "RRi")]
    RealInterval,
    /// Floating-point complex numbers, `CC`.
    #[strum(serialize = "CC")]
    Complex,
    /// Complex intervals, `CCi`.
    #[strum(serialize = "CCi")]
    ComplexInterval,
    /// The two truth values.
    Boolean,
    /// Symbols.
    Symbol,
    /// Strings.
    String,
    /// Nets, the two-dimensional text blocks.
    Net,
    /// The class of `null`.
    Nothing,
}

impl NativeClass {
    /// The native class a domain type names, if it names one.
    pub fn named(name: &ObjectName) -> Option<Self> {
        name.name().parse().ok()
    }

    /// Whether this class is one of the six native number classes.
    pub fn is_number(self) -> bool {
        matches!(
            self,
            Self::Integer
                | Self::Rational
                | Self::Real
                | Self::RealInterval
                | Self::Complex
                | Self::ComplexInterval
        )
    }
}

/// Why the interpreter never calls a method installed for some operands: what
/// it evaluates itself instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeEvaluation {
    /// Arithmetic or comparison on native numbers.
    Numbers,
    /// An operation defined on two integers alone: `//`, `..`, `..<`.
    Integers,
    /// `and`, `or` and `not` on Booleans, which short-circuit.
    Booleans,
    /// Equality and ordering of two strings, symbols, nets or `null`s.
    Text,
    /// Every comparison with a real interval on the left, which the
    /// interpreter answers or rejects itself for any right operand.
    LeftInterval,
    /// The prefix `??` applied to `null`.
    Null,
}

impl NativeEvaluation {
    /// What the interpreter evaluates itself for a binary installation, if
    /// `operator` on these operand classes is evaluated natively.
    pub fn binary(operator: &str, left: NativeClass, right: NativeClass) -> Option<Self> {
        use NativeClass::{Boolean, Integer, Nothing};
        match operator {
            <Token![+]>::SPELLING
            | <Token![-]>::SPELLING
            | <Token![*]>::SPELLING
            | <Token![/]>::SPELLING => Self::both_numbers(left, right),
            <Token![^]>::SPELLING => Self::mixed_numbers(left, right),
            <Token![/ /]>::SPELLING | <Token![..]>::SPELLING | <Token![..<]>::SPELLING => {
                (left == Integer && right == Integer).then_some(Self::Integers)
            }
            <Token![and]>::SPELLING | <Token![or]>::SPELLING => {
                (left == Boolean && right == Boolean).then_some(Self::Booleans)
            }
            <Token![==]>::SPELLING => Self::mixed_numbers(left, right).or_else(|| {
                (left == right && !left.is_number() && left != Nothing).then_some(Self::Text)
            }),
            <Token![?]>::SPELLING => Self::ordered_numbers(left, right)
                .or_else(|| Self::comparable_text(left, right))
                .or_else(|| (left == Nothing && right == Nothing).then_some(Self::Null)),
            _ => None,
        }
    }

    /// What the interpreter evaluates itself for a one-type installation, if
    /// `operator` on this operand class is evaluated natively.
    pub fn unary(operator: &str, operand: NativeClass) -> Option<Self> {
        match operator {
            <Token![+]>::SPELLING | <Token![-]>::SPELLING => {
                operand.is_number().then_some(Self::Numbers)
            }
            <Token![not]>::SPELLING => (operand == NativeClass::Boolean).then_some(Self::Booleans),
            <Token![??]>::SPELLING => (operand == NativeClass::Nothing).then_some(Self::Null),
            _ => None,
        }
    }

    /// What the interpreter evaluates itself for an installation of `operator`
    /// on `domain`, if every domain type is native and the operation is. A real
    /// interval on the left of `==` or `?` is native whatever the right type.
    pub fn of_installation(operator: &str, domain: &[ObjectName]) -> Option<Self> {
        if let [left, _] = domain {
            let comparison =
                operator == <Token![==]>::SPELLING || operator == <Token![?]>::SPELLING;
            if comparison && NativeClass::named(left) == Some(NativeClass::RealInterval) {
                return Some(Self::LeftInterval);
            }
        }
        let classes = domain
            .iter()
            .map(NativeClass::named)
            .collect::<Option<Vec<_>>>()?;
        match classes.as_slice() {
            [left, right] => Self::binary(operator, *left, *right),
            [operand] => Self::unary(operator, *operand),
            _ => None,
        }
    }

    /// Two native numbers.
    fn both_numbers(left: NativeClass, right: NativeClass) -> Option<Self> {
        (left.is_number() && right.is_number()).then_some(Self::Numbers)
    }

    /// Two native numbers, except a complex number against a real interval,
    /// which `^` and `==` leave to method lookup.
    fn mixed_numbers(left: NativeClass, right: NativeClass) -> Option<Self> {
        Self::both_numbers(left, right)
            .filter(|_| (left, right) != (NativeClass::Complex, NativeClass::RealInterval))
    }

    /// Two native numbers that `?` orders itself: nothing is ordered against a
    /// complex interval, and a complex number only against exact or real ones.
    fn ordered_numbers(left: NativeClass, right: NativeClass) -> Option<Self> {
        use NativeClass::{Complex, ComplexInterval, Integer, Rational, Real, RealInterval};
        let ordered = match left {
            Integer | Rational | Real => right.is_number() && right != ComplexInterval,
            Complex => right.is_number() && !matches!(right, RealInterval | ComplexInterval),
            _ => false,
        };
        ordered.then_some(Self::Numbers)
    }

    /// Strings and nets, which `?` orders against each other, and symbols,
    /// which it orders against symbols.
    fn comparable_text(left: NativeClass, right: NativeClass) -> Option<Self> {
        use NativeClass::{Net, String, Symbol};
        let ordered = match left {
            String | Net => matches!(right, String | Net),
            Symbol => right == Symbol,
            _ => false,
        };
        ordered.then_some(Self::Text)
    }

    /// The reason the interpreter never reaches an installed `operator` method.
    pub fn reason(self, operator: &str) -> String {
        match self {
            Self::Numbers => format!(
                "the interpreter evaluates `{operator}` on native numbers itself, before it looks up any method"
            ),
            Self::Integers => format!(
                "the interpreter evaluates `{operator}` on two integers itself, before it looks up any method"
            ),
            Self::Booleans => format!(
                "the interpreter evaluates `{operator}` on Booleans itself, before it looks up any method"
            ),
            Self::Text => format!(
                "the interpreter compares strings, symbols, nets and Booleans with `{operator}` itself, before it looks up any method"
            ),
            Self::LeftInterval => format!(
                "the interpreter evaluates every `{operator}` whose left operand is a real interval itself, and fails on operands it does not know instead of looking up a method"
            ),
            Self::Null => format!(
                "the interpreter evaluates `{operator}` on `null` itself, before it looks up any method"
            ),
        }
    }
}

impl Display for NativeEvaluation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result {
        formatter.write_str(match self {
            Self::Numbers => "native number arithmetic",
            Self::Integers => "integer operation",
            Self::Booleans => "Boolean operation",
            Self::Text => "text comparison",
            Self::LeftInterval => "interval comparison",
            Self::Null => "null operation",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{NativeClass, NativeEvaluation};
    use crate::object_registry::ObjectName;
    use m2_syn::Token;

    /// What the real interpreter ignores, recorded by `tests/native_dispatch`.
    const PROBE: &str = include_str!("../../../tests/native_dispatch/dead_pairs.txt");

    /// The words after `# {label}:` in the probe's header.
    fn probed(label: &str) -> Vec<&'static str> {
        PROBE
            .lines()
            .find_map(|line| line.strip_prefix(&format!("# {label}: ")))
            .unwrap_or_else(|| panic!("the probe header lists {label}"))
            .split_whitespace()
            .collect()
    }

    /// Whether the interpreter ignores installations of `operator` for `classes`.
    fn is_ignored(operator: &str, classes: &[&str]) -> bool {
        let domain: Vec<_> = classes.iter().map(|name| ObjectName::new(*name)).collect();
        NativeEvaluation::of_installation(operator, &domain).is_some()
    }

    /// The table of native evaluations is exactly the set of operand classes
    /// the interpreter was observed to ignore, over every probed operator and
    /// operand class. The binary `??` has its own diagnostic.
    #[test]
    fn table_agrees_with_the_probed_interpreter() {
        let observed: HashSet<Vec<&str>> = PROBE
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| {
                let mut fields: Vec<_> = line.split(" | ").collect();
                fields.pop();
                fields
            })
            .collect();
        let classes = probed("Probed classes");
        let mismatches: Vec<String> = [
            (probed("Probed binary operators"), 2),
            (probed("Probed prefix operators"), 1),
            (probed("Probed postfix operators"), 1),
        ]
        .into_iter()
        .flat_map(|(operators, arity)| {
            let observed = &observed;
            let classes = &classes;
            operators
                .into_iter()
                .filter(move |operator| arity == 1 || *operator != <Token![??]>::SPELLING)
                .flat_map(move |operator| {
                    let domains: Vec<Vec<&str>> = if arity == 2 {
                        classes
                            .iter()
                            .flat_map(|left| classes.iter().map(move |right| vec![*left, *right]))
                            .collect()
                    } else {
                        classes.iter().map(|class| vec![*class]).collect()
                    };
                    domains.into_iter().filter_map(move |domain| {
                        let mut key = vec![operator];
                        key.extend(&domain);
                        let expected = observed.contains(&key);
                        (is_ignored(operator, &domain) != expected).then(|| {
                            format!(
                                "{operator} ({}): the interpreter ignores it: {expected}",
                                domain.join(", ")
                            )
                        })
                    })
                })
        })
        .collect();
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// Classes outside the native set never match, so ordinary installations
    /// stay quiet.
    #[test]
    fn only_native_classes_are_ever_ignored() {
        assert_eq!(NativeClass::named(&ObjectName::new("List")), None);
        assert!(!is_ignored("+", &["ZZ", "Polynomial"]));
        assert!(!is_ignored("+", &["Number", "Number"]));
        assert!(is_ignored("+", &["ZZ", "CC"]));
        assert!(is_ignored("==", &["RRi", "MyType"]));
    }
}
