//! Diagnostic identities and metadata shared by analysis and LSP capabilities.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range as TextRange};

/// Supply the canonical diagnostic declarations to a generating macro.
#[macro_export]
macro_rules! diagnostic_declarations {
    ($consumer:ident) => {
        $consumer! {
            diagnostics {
                node {
                    SyntaxError {
                        code: "X01", name: "syntax-error", severity: ERROR,
                        check: syntax_error,
                    },
                    MissingNode {
                        code: "X02", name: "missing-node", severity: ERROR,
                        check: missing_node,
                    },
                    AmbiguousFloatMemberAccess {
                        code: "X03", name: "ambiguous-float-member-access", severity: WARNING,
                        check: ambiguous_float_member_access,
                        action: ambiguous_float_member_access_action,
                    },
                    MultipleAssignmentTargets {
                        code: "X04", name: "multiple-assignment-targets", severity: ERROR,
                        check: multiple_assignment_targets,
                    },
                    ColonEqualPartAssignment {
                        code: "X05", name: "colon-equal-part-assignment", severity: ERROR,
                        check: colon_equal_part_assignment,
                        action: colon_equal_part_assignment_action,
                    },
                    ParallelAssignmentArity {
                        code: "X06", name: "parallel-assignment-arity", severity: ERROR,
                        check: parallel_assignment_arity,
                    },
                    OptionKeyConvention {
                        code: "S01", name: "option-key-convention", severity: HINT,
                        check: option_key_convention,
                        action: option_key_convention_action,
                    },
                    RedundantControlParentheses {
                        code: "S03", name: "redundant-control-parentheses", severity: HINT,
                        check: redundant_control_parentheses,
                        action: redundant_control_parentheses_action,
                    },
                    PreferCoalescence {
                        code: "S04", name: "prefer-coalescence", severity: HINT,
                        check: prefer_coalescence, action: coalescence_action,
                    },
                    SimplifiableExpression {
                        code: "S05", name: "simplifiable-expression", severity: HINT,
                        check: simplifiable_expression,
                    },
                    RingVariableNaming {
                        code: "S06", name: "ring-variable-naming", severity: HINT,
                        check: ring_variable_naming,
                    },
                }
                document {
                    UnusedBinding {
                        code: "S02", name: "unused-binding", severity: WARNING,
                        check: unused_bindings,
                    },
                }
                installation {
                    InstallNoEffect {
                        code: "E01", name: "install-no-effect", severity: WARNING,
                        check: install_no_effect,
                    },
                    OperatorNotFlexible {
                        code: "E02", name: "operator-not-flexible", severity: ERROR,
                        check: operator_not_flexible,
                    },
                    InstallArity {
                        code: "E03", name: "install-arity", severity: ERROR,
                        check: install_arity,
                    },
                }
                node {
                    InstallNeedsColonEquals {
                        code: "E04", name: "install-needs-colon-equals", severity: ERROR,
                        check: install_needs_colon_equals,
                        action: install_needs_colon_equals_action,
                    },
                    ProtectAssignedSymbol {
                        code: "E05", name: "protect-assigned-symbol", severity: HINT,
                        check: protect_assigned_symbol,
                        action: protect_assigned_symbol_action,
                    },
                    ProtectComputedSymbol {
                        code: "E06", name: "protect-computed-symbol", severity: WARNING,
                        check: protect_computed_symbol,
                    },
                    InvalidControlTransfer {
                        code: "E08", name: "invalid-control-transfer", severity: ERROR,
                        check: invalid_control_transfer,
                    },
                    ExplicitInstallRequired {
                        code: "E09", name: "explicit-install-required", severity: ERROR,
                        check: explicit_install_required,
                    },
                    ParallelAssignmentType {
                        code: "T01", name: "parallel-assignment-type", severity: ERROR,
                        check: parallel_assignment_type,
                    },
                    ConditionType {
                        code: "T02", name: "condition-type", severity: WARNING,
                        check: condition_type,
                    },
                }
                retired {
                    MissingOutputCell {
                        code: "E07", name: "missing-output-cell", severity: WARNING,
                        check: missing_output_cell,
                    },
                }
                codomain {
                    InstallCodomainMissing {
                        code: "T03", name: "install-codomain-missing", severity: HINT,
                        check: missing_codomain,
                        action: method_codomain_action,
                    },
                    InstallCodomainMismatch {
                        code: "T04", name: "install-codomain-mismatch", severity: WARNING,
                        check: codomain_mismatch,
                    },
                }
            }
            standalone_actions {
                ConvertToRawString: convert_to_raw_string_action,
                ConditionalNull: conditional_null_action,
                SimplifyTry: simplify_try_action,
                SimplifyIfCondition: simplify_if_condition_action,
                FlattenElseIf: flatten_else_if_action,
            }
        }
    };
}

struct DiagnosticRegistration {
    code: &'static str,
    name: &'static str,
    severity: DiagnosticSeverity,
}

macro_rules! register_diagnostics {
    (diagnostics { $(
        $phase:ident { $(
            $kind:ident {
                code: $code:literal, name: $name:literal, severity: $severity:ident,
                check: $check:ident
                $(, action: $action:ident)? $(,)?
            }
        ),+ $(,)? }
    )+ } standalone_actions { $($standalone:ident: $standalone_action:ident),* $(,)? }) => {
        /// A diagnostic the server can publish.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum DiagnosticKind {
            $($($kind),+),+
        }

        impl DiagnosticKind {
            pub const ALL: &'static [Self] = &[$($(Self::$kind),+),+];

            fn registration(self) -> DiagnosticRegistration {
                match self {
                    $($(Self::$kind => DiagnosticRegistration {
                        code: $code,
                        name: $name,
                        severity: DiagnosticSeverity::$severity,
                    }),+),+
                }
            }
        }
    };
}

diagnostic_declarations!(register_diagnostics);

impl DiagnosticKind {
    pub fn at(self, range: TextRange, message: impl Into<String>) -> M2Diagnostic {
        M2Diagnostic {
            kind: self,
            range,
            message: message.into(),
        }
    }

    pub fn from_selector(selector: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|diagnostic| {
            let registration = diagnostic.registration();
            selector == registration.code || selector == registration.name
        })
    }

    pub fn from_lsp(diagnostic: &Diagnostic) -> Option<Self> {
        let NumberOrString::String(code) = diagnostic.code.as_ref()? else {
            return None;
        };
        Self::from_selector(code)
    }
}

/// One typed diagnostic finding produced by source analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M2Diagnostic {
    pub kind: DiagnosticKind,
    pub range: TextRange,
    pub message: String,
}

impl M2Diagnostic {
    pub fn to_lsp(&self) -> Diagnostic {
        let registration = self.kind.registration();
        Diagnostic {
            range: self.range,
            severity: Some(registration.severity),
            code: Some(NumberOrString::String(registration.code.to_string())),
            source: Some(registration.name.to_string()),
            message: self.message.clone(),
            ..Default::default()
        }
    }
}

pub trait DiagnosticPolicy {
    fn allows(&self, diagnostic: DiagnosticKind) -> bool;

    fn allows_lsp_diagnostic(&self, diagnostic: &Diagnostic) -> bool {
        DiagnosticKind::from_lsp(diagnostic).is_none_or(|kind| self.allows(kind))
    }
}

pub fn diagnostic_has_kind(diagnostic: &Diagnostic, kind: DiagnosticKind) -> bool {
    DiagnosticKind::from_lsp(diagnostic) == Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn categorized_codes_and_names_are_unique() {
        let mut codes = HashSet::new();
        let mut names = HashSet::new();
        for diagnostic in DiagnosticKind::ALL {
            let registration = diagnostic.registration();
            assert!(
                matches!(
                    registration.code.as_bytes(),
                    [b'D' | b'T' | b'E' | b'X' | b'S', b'0'..=b'9', b'0'..=b'9']
                ),
                "invalid categorized diagnostic code `{}`",
                registration.code
            );
            assert!(codes.insert(registration.code), "duplicate diagnostic code");
            assert!(names.insert(registration.name), "duplicate diagnostic name");
        }
    }

    #[test]
    fn accepts_only_canonical_codes_and_names() {
        assert_eq!(
            DiagnosticKind::from_selector("E01"),
            Some(DiagnosticKind::InstallNoEffect)
        );
        assert_eq!(
            DiagnosticKind::from_selector("install-codomain-mismatch"),
            Some(DiagnosticKind::InstallCodomainMismatch)
        );
        assert_eq!(DiagnosticKind::from_selector("E00"), None);
        assert_eq!(DiagnosticKind::from_selector("E20"), None);
        assert_eq!(DiagnosticKind::from_selector("if-condition-type"), None);
    }
}
