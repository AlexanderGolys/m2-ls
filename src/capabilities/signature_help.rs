//! Signature help: when the cursor is inside a call's argument list, surface the
//! callable's method signatures and highlight the active parameter.
//!
//! The argument list is the right operand of a juxtaposition (SPACE) application
//! `f (a, b)`. We walk outward from the cursor to the innermost such application
//! whose argument subtree contains the cursor, resolve the head's signatures
//! (local method records first, then the builtin/imported index), and report the
//! active parameter as the number of completed arguments before the cursor.

use m2_syn::nodes::{Expr, ExprCollection, ExprOpBinAdj, PackedContent};
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, Position, SignatureHelp, SignatureInformation,
};

use crate::analysis::{Analysis, FunctionInfo, Method};
use crate::document::DocumentSnapshot;
use crate::object_registry::ObjectName;
use crate::record_lsp::{LspKnowledge, ResolvedSignature};
use crate::source::{SourceNavigation, SyntaxSource};
use crate::syntax::ExpressionSyntax;

/// Help response used to render the resolved callable signature.
pub fn signature_help_response(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Option<SignatureHelp> {
    let cursor = document.byte_for_position(position)?;
    let call = CallAtCursor::find(document, cursor, cursor..cursor)?;
    // Only a named (symbol) head can be resolved to recorded signatures; a
    // computed head (`(f g) x`, `obj#k a`) has no name to look up, so we give no
    // signature help for it rather than guess.
    let callable_name = call.left_operand.symbol_name()?;

    let signatures = signature_informations(
        callable_name,
        document.analysis(),
        document.range_for_syntax(call.left_operand.as_ref())?.start,
        knowledge,
    );
    if signatures.is_empty() {
        return None;
    }

    let active_parameter = active_parameter_index(&call.right_operand, cursor, document);
    let active_signature = active_signature_index(&signatures, active_parameter);

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter: Some(active_parameter),
    })
}

/// The active-parameter index: the number of completed arguments whose end falls
/// strictly before the cursor (`f(a, |b)` → 1, the cursor is on the second
/// argument). Strict `<` keeps the cursor on an argument while it sits at that
/// argument's trailing edge (`f(a|, b)` stays on the first argument).
fn active_parameter_index(argument: &Expr, cursor: usize, source: &impl SourceNavigation) -> u32 {
    let inner = match argument {
        Expr::Holder(holder) => match holder.value.as_deref() {
            Some(value) => value,
            None => return 0,
        },
        _ => argument,
    };
    match inner {
        Expr::Collection(ExprCollection::Sequence(sequence)) => match &sequence.elements {
            Some(PackedContent::Expr(expression)) => u32::from(
                source
                    .syntax_bytes(expression.as_ref())
                    .is_some_and(|bytes| bytes.end < cursor),
            ),
            Some(PackedContent::Pack(pack)) => pack
                .pack
                .iter()
                .filter(|component| {
                    source
                        .syntax_bytes(*component)
                        .is_some_and(|bytes| bytes.end < cursor)
                })
                .count() as u32,
            None => 0,
        },
        _ => u32::from(
            source
                .syntax_bytes(inner)
                .is_some_and(|bytes| bytes.end < cursor),
        ),
    }
}

/// Locates the innermost typed call whose argument contains the cursor.
pub struct CallAtCursor<'ast, 'source> {
    /// Document providing current cell offsets.
    document: &'source DocumentSnapshot,
    /// Cursor offset in UTF-8 bytes.
    cursor: usize,
    /// Syntax that must be enclosed by the call argument.
    anchor: std::ops::Range<usize>,
    /// Innermost call found so far.
    call: Option<&'ast ExprOpBinAdj>,
}

impl<'ast> CallAtCursor<'ast, '_> {
    /// Finds the innermost call containing the cursor and all of `anchor` in its argument.
    pub fn find(
        document: &'ast DocumentSnapshot,
        cursor: usize,
        anchor: std::ops::Range<usize>,
    ) -> Option<&'ast ExprOpBinAdj> {
        let syntax = document.syntax();
        let mut query = CallAtCursor {
            document,
            cursor,
            anchor,
            call: None,
        };
        for cell in syntax.cells_intersecting(cursor..cursor) {
            if let Some(cell) = cell.syntax() {
                query.visit_cell(cell);
            }
        }
        query.call
    }
}

impl<'ast> Visit<'ast> for CallAtCursor<'ast, '_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self
            .document
            .syntax_bytes(expression)
            .is_none_or(|range| self.cursor < range.start || self.cursor > range.end)
        {
            return;
        }
        visit::visit_expr(self, expression);
    }

    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        if self
            .document
            .syntax_bytes(call.right_operand.as_ref())
            .is_some_and(|range| {
                range.start <= self.cursor
                    && self.cursor <= range.end
                    && range.start <= self.anchor.start
                    && self.anchor.end <= range.end
            })
        {
            self.call = Some(call);
        }
        visit::visit_expr_op_bin_adj(self, call);
    }
}

/// The signature whose parameter count first covers the active parameter, so the
/// editor highlights a signature that actually has that slot. Falls back to the
/// first signature.
fn active_signature_index(signatures: &[SignatureInformation], active_parameter: u32) -> usize {
    signatures
        .iter()
        .position(|signature| {
            signature
                .parameters
                .as_ref()
                .is_some_and(|parameters| parameters.len() as u32 > active_parameter)
        })
        .unwrap_or(0)
}

/// The signatures for `callable`: a local method function's recorded methods
/// take precedence (user code shadows builtins); otherwise the builtin/imported
/// index's signatures, both documented (with a codomain) and undocumented
/// installed methods (domain only).
fn signature_informations(
    callable: &str,
    analysis: &Analysis,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Vec<SignatureInformation> {
    if let Some(function) = analysis.function_at(callable, position) {
        let local = local_signatures(callable, analysis, function);
        if !local.is_empty() {
            return local;
        }
    }

    let Some(record) = knowledge.get_record(&ObjectName::new(callable)) else {
        return Vec::new();
    };
    // Documented signatures carry a codomain; undocumented installed methods
    // (the rest) carry only a domain. Both are real call shapes worth showing.
    let mut signatures: Vec<SignatureInformation> = knowledge
        .documented_signatures(record)
        .iter()
        .map(|signature| builtin_signature_information(callable, signature))
        .collect();
    for method in knowledge.undocumented_installed_methods(record) {
        let domain = method
            .domain
            .iter()
            .filter_map(|object_id| knowledge.object(object_id).map(|record| &record.name))
            .map(|name| name.0.clone())
            .collect::<Vec<_>>();
        signatures.push(signature_information(callable, &domain, None));
    }
    signatures
}

/// The domain types of an installed method signature `[callable, D1, D2, …]` —
/// the parts after the leading callable name.
fn method_domain(signature: &[ObjectName]) -> Vec<String> {
    signature.iter().skip(1).map(|id| id.0.clone()).collect()
}

/// Signatures from a locally-defined method function's installed methods.
fn local_signatures(
    callable: &str,
    analysis: &Analysis,
    function: &FunctionInfo,
) -> Vec<SignatureInformation> {
    analysis
        .effective_installations(function)
        .map(|installation| local_signature_information(callable, &installation.method))
        .collect()
}

/// Signature information selected from the callable and binding facts of this document.
fn local_signature_information(callable: &str, method: &Method) -> SignatureInformation {
    let domain = method
        .domain
        .iter()
        .map(ObjectName::name)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    signature_information(
        callable,
        &domain,
        method.codomain.as_ref().map(ObjectName::name),
    )
}

/// Protocol signature labels and parameter information for a builtin callable.
fn builtin_signature_information(
    callable: &str,
    signature: &ResolvedSignature,
) -> SignatureInformation {
    // `signature.signature` is `[callable, D1, D2, …]`; the parameters are the
    // domain after the leading callable name.
    let domain = method_domain(&signature.signature);
    let codomain = signature.output_types.first().map(|id| id.0.as_str());
    signature_information(callable, &domain, codomain)
}

/// Build one `SignatureInformation` with label `callable(D1, D2) -> Codomain`
/// and one parameter per domain type.
fn signature_information(
    callable: &str,
    domain: &[String],
    codomain: Option<&str>,
) -> SignatureInformation {
    let parameter_list = domain.join(", ");
    let label = match codomain {
        Some(codomain) => format!("{callable}({parameter_list}) -> {codomain}"),
        None => format!("{callable}({parameter_list})"),
    };
    let parameters = domain
        .iter()
        .map(|type_name| ParameterInformation {
            label: ParameterLabel::Simple(type_name.clone()),
            documentation: None,
        })
        .collect();
    SignatureInformation {
        label,
        documentation: None,
        parameters: Some(parameters),
        active_parameter: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn help(text: &str, position: Position) -> Option<SignatureHelp> {
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let scoped = index.with_source_imports(text);
        signature_help_response(&document, position, &scoped)
    }

    fn argument_texts(text: &str) -> Vec<String> {
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let call =
            CallAtCursor::find(&document, 2, 2..2).expect("fixture should contain an application");
        call.right_operand
            .arguments()
            .into_iter()
            .map(|argument| {
                argument
                    .and_then(|expression| document.text_for_syntax(expression))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn argument_slots_keep_empty_components_and_skip_muted_expressions() {
        assert_eq!(argument_texts("f(,x,,)\n"), vec!["", "x", "", ""]);
        assert_eq!(argument_texts("f(ignored;x,y)\n"), vec!["x", "y"]);
    }

    #[test]
    fn local_method_signatures_are_reported() {
        // Cursor inside the argument list of a call to a local method function.
        let text = "f = method()\nf(ZZ, String) := (n, s) -> n\nf(1, \"a\")\n";
        let response = help(text, pos!(2, 2)).expect("signature help in the argument list");
        assert!(
            response
                .signatures
                .iter()
                .any(|signature| signature.label.contains("f(ZZ, String)")),
            "got {:?}",
            response
                .signatures
                .iter()
                .map(|signature| &signature.label)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn active_parameter_tracks_the_cursor() {
        let text = "f = method()\nf(ZZ, String) := (n, s) -> n\nf(1, \"a\")\n";
        // Cursor on the first argument `1`.
        let first = help(text, pos!(2, 2)).expect("signature help");
        assert_eq!(first.active_parameter, Some(0));
        // Cursor on the second argument `\"a\"` (after the comma).
        let second = help(text, pos!(2, 5)).expect("signature help");
        assert_eq!(second.active_parameter, Some(1));
    }

    #[test]
    fn active_parameter_counts_empty_slots_and_nested_calls() {
        let text = "concatenate(, \"a\",, \"b\")\n";
        for (character, expected) in [(12, 0), (15, 1), (18, 2), (20, 3)] {
            let response = help(text, pos!(0, character)).expect("signature help");
            assert_eq!(response.active_parameter, Some(expected), "at {character}");
        }
        let nested = "concatenate(\"a\", concatenate(\"b\", \"c\"))\n";
        let response = help(nested, pos!(0, 35)).expect("nested signature help");
        assert_eq!(response.active_parameter, Some(1));
    }

    #[test]
    fn call_query_can_anchor_an_enclosing_option() {
        let text = "outer(Option => inner(Value))\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("snapshot");
        let cursor = text.find("Value").expect("value") + 2;
        let option = text.find("Option").expect("option")..text.find("))").expect("closing") + 1;
        let inner = CallAtCursor::find(&document, cursor, cursor..cursor).expect("inner call");
        assert_eq!(inner.left_operand.symbol_name(), Some("inner"));
        let outer = CallAtCursor::find(&document, cursor, option).expect("option's call");
        assert_eq!(outer.left_operand.symbol_name(), Some("outer"));
    }

    #[test]
    fn cursor_on_the_callable_gives_no_signature_help() {
        // The cursor is on `f`, not inside its argument list.
        let text = "f = method()\nf(ZZ, String) := (n, s) -> n\nf(1, \"a\")\n";
        assert!(help(text, pos!(2, 0)).is_none());
    }

    #[test]
    fn builtin_callable_signatures_are_reported() {
        // `concatenate` is a Core function; its signatures resolve from the index.
        let text = "concatenate(\"a\", \"b\")\n";
        let response = help(text, pos!(0, 12)).expect("signature help for a builtin");
        assert!(!response.signatures.is_empty());
        // The label is `concatenate(Domain…)` — the callable name must not be
        // repeated as its own first parameter.
        assert!(
            response
                .signatures
                .iter()
                .all(|signature| !signature.label.contains("concatenate(concatenate")),
            "got {:?}",
            response
                .signatures
                .iter()
                .map(|signature| &signature.label)
                .collect::<Vec<_>>()
        );
        assert!(response
            .signatures
            .iter()
            .any(|signature| signature.label.starts_with("concatenate(")),);
    }

    #[test]
    fn imported_callable_signatures_use_the_owning_partition() {
        let text = "needsPackage \"JSON\"\ntoJSON(x)\n";
        let response = help(text, pos!(1, 7)).expect("signature help for an imported callable");

        assert!(response
            .signatures
            .iter()
            .any(|signature| signature.label == "toJSON(Symbol) -> String"));
    }
}
