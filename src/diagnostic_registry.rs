//! The catalog of diagnostics m2-ls can report.
//!
//! Every diagnostic is one [`DiagnosticKind`], registered once in the table
//! below with the code, name, and severity it is published under. Analysis
//! creates findings with [`DiagnosticKind::at`], publication converts them with
//! [`M2Diagnostic::to_lsp`], and settings and code actions read a kind back
//! from user text or a client's diagnostic with [`DiagnosticKind::from_selector`]
//! and [`DiagnosticKind::from_lsp`].
//!
//! # Codes
//!
//! A code is a category letter followed by two digits:
//!
//! | Letter | Category |
//! | --- | --- |
//! | `X` | malformed syntax or assignment structure |
//! | `S` | style and simplification hints |
//! | `E` | code Macaulay2 rejects or silently ignores when it runs |
//! | `T` | type errors found by inference |
//!
//! Users write codes and names in their settings, so neither ever changes, and
//! a removed diagnostic's code is never reused: `E07` belonged to the
//! `missing-output-cell` warning, removed in 1.1.0.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range as TextRange};

/// The stable code a diagnostic is published under, such as `"S02"`.
type DiagnosticCode = &'static str;

/// The stable kebab-case name of a diagnostic, such as `"unused-binding"`.
type DiagnosticName = &'static str;

/// Declares [`DiagnosticKind`] from the registration table: each entry becomes
/// one variant, documented by the entry's doc comment, together with its code,
/// name, and severity.
macro_rules! register_diagnostics {
    ($(
        $(#[doc = $doc:literal])*
        $kind:ident { code: $code:literal, name: $name:literal, severity: $severity:ident }
    )+) => {
        /// One diagnostic m2-ls can report.
        ///
        /// Each variant's documentation shows Macaulay2 source that triggers it.
        /// A check creates its finding by naming the kind:
        ///
        /// ```
        /// let finding = DiagnosticKind::UnusedBinding.at(range, "Unused variable total");
        /// ```
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum DiagnosticKind {
            $($(#[doc = $doc])* $kind,)+
        }

        impl DiagnosticKind {
            /// Every registered diagnostic, in registration order.
            pub const ALL: &'static [Self] = &[$(Self::$kind),+];

            /// The code this diagnostic is published under.
            fn code(self) -> DiagnosticCode {
                match self {
                    $(Self::$kind => $code,)+
                }
            }

            /// The name users may write instead of the code.
            fn name(self) -> DiagnosticName {
                match self {
                    $(Self::$kind => $name,)+
                }
            }

            /// How prominently clients display this diagnostic.
            fn severity(self) -> DiagnosticSeverity {
                match self {
                    $(Self::$kind => DiagnosticSeverity::$severity,)+
                }
            }
        }
    };
}

register_diagnostics! {
    /// Source the grammar cannot parse.
    ///
    /// ```m2
    /// x = (1 + ]
    /// ```
    SyntaxError { code: "X01", name: "syntax-error", severity: ERROR }

    /// A token the grammar expected but did not find, such as an `if` that
    /// ends after `then`.
    ///
    /// ```m2
    /// y = if true then
    /// ```
    MissingNode { code: "X02", name: "missing-node", severity: ERROR }

    /// `L.1` reads as `L` applied to the float `.1`, not as member access;
    /// the fix suggests `L#1`.
    ///
    /// ```m2
    /// L = {10, 20}
    /// second = L.1
    /// ```
    AmbiguousFloatMemberAccess { code: "X03", name: "ambiguous-float-member-access", severity: WARNING }

    /// A parallel assignment target M2 cannot assign. Targets are assigned
    /// component-wise and may be symbols, nested lists, parts such as `x#i`
    /// (with `=`), or installation targets such as `T + T` or `f ZZ`; under
    /// `:=` an operator target must be an installation, so a literal operand
    /// rules it out.
    ///
    /// ```m2
    /// (a, 1) = (2, 3)
    /// ```
    MultipleAssignmentTargets { code: "X04", name: "multiple-assignment-targets", severity: ERROR }

    /// `:=` used to assign a part; parts are assigned with `=`.
    ///
    /// ```m2
    /// L = new MutableList from {1, 2}
    /// L#0 := 5
    /// ```
    ColonEqualPartAssignment { code: "X05", name: "colon-equal-part-assignment", severity: ERROR }

    /// A parallel assignment whose two sides list different numbers of
    /// elements.
    ///
    /// ```m2
    /// (a, b) = (1, 2, 3)
    /// ```
    ParallelAssignmentArity { code: "X06", name: "parallel-assignment-arity", severity: ERROR }

    /// A lowercase option key in a call; Macaulay2 option names are
    /// capitalized.
    ///
    /// ```m2
    /// y = g(1, verbose => true)
    /// ```
    OptionKeyConvention { code: "S01", name: "option-key-convention", severity: HINT }

    /// A local binding that is never read. Names starting with `_` are exempt.
    ///
    /// ```m2
    /// f = () -> (
    ///     unused := 1;
    ///     2
    ///     )
    /// ```
    UnusedBinding { code: "S02", name: "unused-binding", severity: WARNING }

    /// Parentheses around a condition, `try` value, or loop range that the
    /// keyword already delimits.
    ///
    /// ```m2
    /// f = n -> if (n > 0) then 1 else 2
    /// ```
    RedundantControlParentheses { code: "S03", name: "redundant-control-parentheses", severity: HINT }

    /// A null test that `??` expresses directly; here `x ?? 0`.
    ///
    /// ```m2
    /// f = x -> if x === null then 0 else x
    /// ```
    PreferCoalescence { code: "S04", name: "prefer-coalescence", severity: HINT }

    /// An `if` or `try` with a shorter equivalent form: a redundant `null`
    /// branch, a negated condition, or a nesting that flattens into an
    /// `else if` chain.
    ///
    /// ```m2
    /// f = n -> if n > 0 then 1 else null
    /// ```
    SimplifiableExpression { code: "S05", name: "simplifiable-expression", severity: HINT }

    /// A multi-character ring variable such as `x0`, which is a separate
    /// symbol rather than the indexed variable `x_0`.
    ///
    /// ```m2
    /// R = QQ[x0, x1]
    /// ```
    RingVariableNaming { code: "S06", name: "ring-variable-naming", severity: HINT }

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
    InstallNoEffect { code: "E01", name: "install-no-effect", severity: WARNING }

    /// A method installed on an operator form Macaulay2 does not let users
    /// extend.
    ///
    /// ```m2
    /// ZZ === ZZ := (a, b) -> true
    /// ```
    OperatorNotFlexible { code: "E02", name: "operator-not-flexible", severity: ERROR }

    /// A method function whose parameter count differs from the installed
    /// domain.
    ///
    /// ```m2
    /// g = method()
    /// g(ZZ, ZZ) := (a, b, c) -> a
    /// ```
    InstallArity { code: "E03", name: "install-arity", severity: ERROR }

    /// A method installed with `=`, which Macaulay2 rejects; installations use
    /// `:=`.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ = n -> n
    /// ```
    InstallNeedsColonEquals { code: "E04", name: "install-needs-colon-equals", severity: ERROR }

    /// `protect x` on a bound name protects whatever symbol `x` evaluates to;
    /// `protect symbol x` protects `x` itself.
    ///
    /// ```m2
    /// x = 1
    /// protect x
    /// ```
    ProtectAssignedSymbol { code: "E05", name: "protect-assigned-symbol", severity: HINT }

    /// `protect` applied to an expression, so the protected symbol is only
    /// known at run time.
    ///
    /// ```m2
    /// protect getSymbol "x"
    /// ```
    ProtectComputedSymbol { code: "E06", name: "protect-computed-symbol", severity: WARNING }

    /// `return`, `break`, or `continue` outside the function or loop it
    /// needs.
    ///
    /// ```m2
    /// f = n -> (if n > 0 then break 1; n)
    /// ```
    InvalidControlTransfer { code: "E08", name: "invalid-control-transfer", severity: ERROR }

    /// `x <- y` used as an assignment target, alone or inside a parallel
    /// assignment. That never installs a callable method, so it is always an
    /// error; `<-` is overloaded only with
    /// `installMethod(symbol <-, Type, function)`.
    ///
    /// ```m2
    /// (x <- ZZ) := y -> y
    /// ```
    ExplicitInstallRequired { code: "E09", name: "explicit-install-required", severity: ERROR }

    /// A parallel assignment whose right side has a type that cannot supply
    /// several values.
    ///
    /// ```m2
    /// (a, b) = 5
    /// ```
    ParallelAssignmentType { code: "T01", name: "parallel-assignment-type", severity: ERROR }

    /// An `if` or `while` condition whose type is known not to be `Boolean`.
    ///
    /// ```m2
    /// f = n -> if #n then 1 else 2
    /// ```
    ConditionType { code: "T02", name: "condition-type", severity: WARNING }

    /// A method installation without a codomain where inference can deduce
    /// one; the fix inserts `ZZ =>` here.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := n -> n + 1
    /// ```
    InstallCodomainMissing { code: "T03", name: "install-codomain-missing", severity: HINT }

    /// A method installation whose declared codomain contradicts the inferred
    /// result type.
    ///
    /// ```m2
    /// g = method()
    /// g ZZ := String => n -> n + 1
    /// ```
    InstallCodomainMismatch { code: "T04", name: "install-codomain-mismatch", severity: WARNING }
}

impl DiagnosticKind {
    /// A finding of this kind covering `range`, worded by `message`.
    ///
    /// ```
    /// let finding = DiagnosticKind::ConditionType.at(
    ///     source.range_for_node(condition),
    ///     "if condition must have type `Boolean`, but this expression has type `ZZ`",
    /// );
    /// ```
    pub fn at(self, range: TextRange, message: impl Into<String>) -> M2Diagnostic {
        M2Diagnostic {
            kind: self,
            range,
            message: message.into(),
        }
    }

    /// The diagnostic a user names in their settings, by code or by name.
    ///
    /// ```
    /// assert_eq!(DiagnosticKind::from_selector("S02"), Some(DiagnosticKind::UnusedBinding));
    /// assert_eq!(DiagnosticKind::from_selector("unused-binding"), Some(DiagnosticKind::UnusedBinding));
    /// assert_eq!(DiagnosticKind::from_selector("E07"), None);
    /// ```
    pub fn from_selector(selector: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| selector == kind.code() || selector == kind.name())
    }

    /// The kind of a diagnostic a client sends back with a code-action
    /// request; `None` for diagnostics another server published.
    ///
    /// ```
    /// let published = DiagnosticKind::UnusedBinding.at(range, "Unused variable total").to_lsp();
    /// assert_eq!(DiagnosticKind::from_lsp(&published), Some(DiagnosticKind::UnusedBinding));
    /// ```
    pub fn from_lsp(diagnostic: &Diagnostic) -> Option<Self> {
        let NumberOrString::String(code) = diagnostic.code.as_ref()? else {
            return None;
        };
        Self::from_selector(code)
    }
}

/// One problem analysis found in a document.
///
/// It is an LSP [`Diagnostic`] before publication, carrying the typed
/// [`DiagnosticKind`] instead of a string code, so settings can filter findings
/// and tests can match them without parsing codes back.
///
/// ```
/// let finding = DiagnosticKind::UnusedBinding.at(range, "Unused variable total");
/// assert_eq!(finding.kind, DiagnosticKind::UnusedBinding);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M2Diagnostic {
    /// Which diagnostic this finding is.
    pub kind: DiagnosticKind,
    /// The source span the client underlines.
    pub range: TextRange,
    /// The explanation shown to the reader, ideally stating the fix.
    pub message: String,
}

impl M2Diagnostic {
    /// The finding as published to clients, under its kind's code, name, and
    /// severity.
    ///
    /// ```
    /// let published = DiagnosticKind::UnusedBinding.at(range, "Unused variable total").to_lsp();
    /// assert_eq!(published.code, Some(NumberOrString::String("S02".to_string())));
    /// assert_eq!(published.source.as_deref(), Some("unused-binding"));
    /// ```
    pub fn to_lsp(&self) -> Diagnostic {
        Diagnostic {
            range: self.range,
            severity: Some(self.kind.severity()),
            code: Some(NumberOrString::String(self.kind.code().to_string())),
            source: Some(self.kind.name().to_string()),
            message: self.message.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_follow_the_category_scheme_and_codes_and_names_are_unique() {
        let mut codes = HashSet::new();
        let mut names = HashSet::new();
        for kind in DiagnosticKind::ALL {
            assert!(
                matches!(
                    kind.code().as_bytes(),
                    [b'X' | b'S' | b'E' | b'T', b'0'..=b'9', b'0'..=b'9']
                ),
                "invalid diagnostic code `{}`",
                kind.code()
            );
            assert!(codes.insert(kind.code()), "duplicate diagnostic code");
            assert!(names.insert(kind.name()), "duplicate diagnostic name");
        }
    }

    #[test]
    fn selectors_accept_only_registered_codes_and_names() {
        assert_eq!(
            DiagnosticKind::from_selector("E01"),
            Some(DiagnosticKind::InstallNoEffect)
        );
        assert_eq!(
            DiagnosticKind::from_selector("install-codomain-mismatch"),
            Some(DiagnosticKind::InstallCodomainMismatch)
        );
        assert_eq!(DiagnosticKind::from_selector("E00"), None);
        assert_eq!(DiagnosticKind::from_selector("E07"), None);
        assert_eq!(DiagnosticKind::from_selector("if-condition-type"), None);
    }
}
