//! The registration table: every diagnostic m2-ls can report, as one
//! [`DiagnosticRule`] value with the code, name, severity, and tags it is
//! published under.
//!
//! Each entry's documentation shows Macaulay2 source that triggers it; the
//! checks that spawn it run on the occasions in [`super::checks`].

use tower_lsp::lsp_types::{DiagnosticSeverity, DiagnosticTag};

use super::{DiagnosticCode, DiagnosticRule};

/// Declares one `static` rule per entry, documented by the entry's doc comment,
/// and lists every rule in registration order.
macro_rules! register_diagnostics {
    ($(
        $(#[doc = $doc:literal])*
        $rule:ident {
            code: $code:literal,
            name: $name:literal,
            severity: $severity:ident,
            $(tags: [$($tag:ident),+],)?
        }
    )+) => {
        $(
            $(#[doc = $doc])*
            pub static $rule: DiagnosticRule = DiagnosticRule {
                code: DiagnosticCode($code),
                name: $name,
                severity: DiagnosticSeverity::$severity,
                tags: &[$($(DiagnosticTag::$tag),+)?],
            };
        )+

        /// Every registered rule, in registration order.
        pub static RULES: &[&DiagnosticRule] = &[$(&$rule),+];
    };
}

register_diagnostics! {
    /// Source the grammar cannot parse.
    ///
    /// ```m2
    /// x = (1 + ]
    /// ```
SYNTAX_ERROR {
        code: "X01",
        name: "syntax-error",
        severity: ERROR,
    }

    /// A token the grammar expected but did not find, such as an `if` that
    /// ends after `then`.
    ///
    /// ```m2
    /// y = if true then
    /// ```
MISSING_NODE {
        code: "X02",
        name: "missing-node",
        severity: ERROR,
    }

    /// `L.1` reads as `L` applied to the float `.1`, not as member access;
    /// the fix suggests `L#1`.
    ///
    /// ```m2
    /// L = {10, 20}
    /// second = L.1
    /// ```
AMBIGUOUS_FLOAT_MEMBER_ACCESS {
        code: "X03",
        name: "ambiguous-float-member-access",
        severity: WARNING,
    }

    /// A parallel assignment target M2 cannot assign. Targets are assigned
    /// component-wise and may be symbols, nested lists, parts such as `x#i`
    /// (with `=`), or installation targets such as `T + T` or `f ZZ`; under
    /// `:=` an operator target must be an installation, so a literal operand
    /// rules it out.
    ///
    /// ```m2
    /// (a, 1) = (2, 3)
    /// ```
MULTIPLE_ASSIGNMENT_TARGETS {
        code: "X04",
        name: "multiple-assignment-targets",
        severity: ERROR,
    }

    /// `:=` used to assign a part; parts are assigned with `=`.
    ///
    /// ```m2
    /// L = new MutableList from {1, 2}
    /// L#0 := 5
    /// ```
COLON_EQUAL_PART_ASSIGNMENT {
        code: "X05",
        name: "colon-equal-part-assignment",
        severity: ERROR,
    }

    /// A parallel assignment whose two sides list different numbers of
    /// elements.
    ///
    /// ```m2
    /// (a, b) = (1, 2, 3)
    /// ```
PARALLEL_ASSIGNMENT_ARITY {
        code: "X06",
        name: "parallel-assignment-arity",
        severity: ERROR,
    }

    /// A lowercase option key in a call; Macaulay2 option names are
    /// capitalized.
    ///
    /// ```m2
    /// y = g(1, verbose => true)
    /// ```
OPTION_KEY_CONVENTION {
        code: "S01",
        name: "option-key-convention",
        severity: HINT,
    }

    /// A local binding that is never read. Names starting with `_` are exempt.
    ///
    /// ```m2
    /// f = () -> (
    ///     unused := 1;
    ///     2
    ///     )
    /// ```
UNUSED_BINDING {
        code: "S02",
        name: "unused-binding",
        severity: WARNING,
        tags: [UNNECESSARY],
    }

    /// Parentheses around a condition, `try` value, or loop range that the
    /// keyword already delimits.
    ///
    /// ```m2
    /// f = n -> if (n > 0) then 1 else 2
    /// ```
REDUNDANT_CONTROL_PARENTHESES {
        code: "S03",
        name: "redundant-control-parentheses",
        severity: HINT,
    }

    /// A null test that `??` expresses directly: here `x ?? 0`, and
    /// `typeInfo ??= class value` for an `if` that only assigns the fallback.
    ///
    /// ```m2
    /// f = x -> if x === null then 0 else x
    /// ```
    ///
    /// ```m2
    /// if typeInfo === null then typeInfo = class value;
    /// ```
PREFER_COALESCENCE {
        code: "S04",
        name: "prefer-coalescence",
        severity: HINT,
    }

    /// An `if` or `try` with a shorter equivalent form: a redundant `null`
    /// branch, a negated condition, or a nesting that flattens into an
    /// `else if` chain.
    ///
    /// ```m2
    /// f = n -> if n > 0 then 1 else null
    /// ```
SIMPLIFIABLE_EXPRESSION {
        code: "S05",
        name: "simplifiable-expression",
        severity: HINT,
    }

    /// A multi-character ring variable such as `x0`, which is a separate
    /// symbol rather than the indexed variable `x_0`.
    ///
    /// ```m2
    /// R = QQ[x0, x1]
    /// ```
RING_VARIABLE_NAMING {
        code: "S06",
        name: "ring-variable-naming",
        severity: HINT,
    }

    /// A method installed on something that does not dispatch to it: a plain
    /// function rather than a `method()`, the binary form of `??`, or a `<-`
    /// method for anything but one non-symbol type. M2 evaluates `x <- v` by
    /// assigning a symbol `x` directly and otherwise calling the one-type
    /// method of `class x`, yet `installMethod` accepts every other domain.
    ///
    /// ```m2
    /// f = x -> x
    /// f ZZ := n -> n
    /// ```
    ///
    /// ```m2
    /// installMethod(symbol <-, Symbol, Symbol, (x, y, v) -> v)
    /// ```
INSTALL_NO_EFFECT {
        code: "E01",
        name: "install-no-effect",
        severity: WARNING,
    }

    /// A method installed on an operator form Macaulay2 does not let users
    /// extend.
    ///
    /// ```m2
    /// ZZ === ZZ := (a, b) -> true
    /// ```
OPERATOR_NOT_FLEXIBLE {
        code: "E02",
        name: "operator-not-flexible",
        severity: ERROR,
    }

    /// A method function whose parameter count differs from the installed
    /// domain.
    ///
    /// ```m2
    /// g = method()
    /// g(ZZ, ZZ) := (a, b, c) -> a
    /// ```
INSTALL_ARITY {
        code: "E03",
        name: "install-arity",
        severity: ERROR,
    }

    /// A method installed with `=`, which Macaulay2 rejects; installations use
    /// `:=`.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ = n -> n
    /// ```
INSTALL_NEEDS_COLON_EQUALS {
        code: "E04",
        name: "install-needs-colon-equals",
        severity: ERROR,
    }

    /// `protect` applied to an expression, so the protected symbol is only
    /// known at run time.
    ///
    /// ```m2
    /// protect getSymbol "x"
    /// ```
PROTECT_COMPUTED_SYMBOL {
        code: "E06",
        name: "protect-computed-symbol",
        severity: WARNING,
    }

    /// `return`, `break`, or `continue` outside the function or loop it
    /// needs.
    ///
    /// ```m2
    /// f = n -> (if n > 0 then break 1; n)
    /// ```
INVALID_CONTROL_TRANSFER {
        code: "E08",
        name: "invalid-control-transfer",
        severity: ERROR,
    }

    /// `x <- y` used as an assignment target, alone or inside a parallel
    /// assignment. That never installs a callable method, so it is always an
    /// error; `<-` is overloaded only with
    /// `installMethod(symbol <-, Type, function)`.
    ///
    /// ```m2
    /// (x <- ZZ) := y -> y
    /// ```
EXPLICIT_INSTALL_REQUIRED {
        code: "E09",
        name: "explicit-install-required",
        severity: ERROR,
    }

    /// A parallel assignment whose right side has a type that cannot supply
    /// several values.
    ///
    /// ```m2
    /// (a, b) = 5
    /// ```
PARALLEL_ASSIGNMENT_TYPE {
        code: "T01",
        name: "parallel-assignment-type",
        severity: ERROR,
    }

    /// An `if` or `while` condition whose type is known not to be `Boolean`.
    ///
    /// ```m2
    /// f = n -> if #n then 1 else 2
    /// ```
CONDITION_TYPE {
        code: "T02",
        name: "condition-type",
        severity: WARNING,
    }

    /// A method installation without a codomain where inference can deduce
    /// one; the fix inserts `ZZ =>` here.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := n -> n + 1
    /// ```
INSTALL_CODOMAIN_MISSING {
        code: "T03",
        name: "install-codomain-missing",
        severity: HINT,
    }

    /// A method installation whose declared codomain contradicts the inferred
    /// result type.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := String => n -> n + 1
    /// ```
INSTALL_CODOMAIN_MISMATCH {
        code: "T04",
        name: "install-codomain-mismatch",
        severity: WARNING,
    }
}
