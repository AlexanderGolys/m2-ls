//! The installation occasion: diagnostics spawned for each method installation
//! the analysis characterized.

use m2_syn::Token;

use super::{DiagnosticSink, InstallationContext};
use crate::analysis::{
    CallableHeadKind, Dispatch, InstalledValue, MethodHead, MethodInstallationSyntax,
    MethodOptions, Operator,
};
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::matches_token;
use crate::object_registry::OperatorForm;
use crate::typesystem::{TypeKnowledge, TypeRole};

/// Spawns every diagnostic found for the context's installation.
pub(super) fn spawn(context: &InstallationContext, sink: &mut DiagnosticSink) {
    install_no_effect(context, sink);
    operator_not_flexible(context, sink);
    install_arity(context, sink);
    install_option_form(context, sink);
}

/// Reports method installations that cannot affect the callable they target.
fn install_no_effect(context: &InstallationContext, sink: &mut DiagnosticSink) {
    let installation = context.installation;
    let method = &installation.method;
    match &method.head {
        MethodHead::Operator(operator)
            if operator.form == OperatorForm::Binary
                && matches_token::<Token![??]>(operator.token.name())
                && installation.expected_argument_arity() == method.domain.len() =>
        {
            sink.spawn(
                &catalog::INSTALL_NO_EFFECT,
                Finding::new(
                    installation.span,
                    "Installing a binary `??` method has no effect: M2 records the method, but `x ?? y` never dispatches to it. Install the prefix form `?? X := x -> ...` to customize how `X` behaves on the left of `??`.",
                ),
            );
        }
        MethodHead::Operator(operator) if matches_token::<Token![<-]>(operator.token.name()) => {
            if let Some(message) = left_arrow_install_without_effect(context) {
                sink.spawn(
                    &catalog::INSTALL_NO_EFFECT,
                    Finding::new(installation.span, message),
                );
            }
        }
        MethodHead::Function(name) => {
            let start = installation.span.start;
            if context
                .analysis
                .callable_head_kind(name.name(), start, context.knowledge)
                != CallableHeadKind::PlainFunction
            {
                return;
            }
            let finding = Finding::new(
                installation.span,
                format!(
                    "Installing a method on `{name}` has no effect: `{name}` is not a method \
                     function. Define it with `{name} = method()` to make method installations take effect."
                ),
            );
            sink.spawn(
                &catalog::INSTALL_NO_EFFECT,
                match context.analysis.get_binding_at(name.name(), start) {
                    Some(definition) => finding.with_note(
                        definition.range,
                        format!("`{name}` is defined here as a plain function"),
                    ),
                    None => finding,
                },
            );
        }
        MethodHead::Operator(_) => {}
    }
}

/// Why a `<-` installation is never called, if it is not. M2 evaluates
/// `x <- v` by assigning a symbol `x` directly and otherwise looking up the
/// one-type method `symbol <-` in the class of `x`
/// (`assigntofun` in M2's `d/evaluate.d`), so only a single non-symbol type
/// can receive the call; `installMethod` still accepts any other domain.
fn left_arrow_install_without_effect(context: &InstallationContext) -> Option<String> {
    let domain = &context.installation.method.domain;
    let [target] = domain.as_slice() else {
        return Some(format!(
            "Installing a `<-` method for {} types has no effect: M2 dispatches `x <- v` on the \
             class of `x` alone, so this method is stored but never called. Install it for one \
             type with `installMethod(symbol <-, Type, (x, v) -> ...)`.",
            domain.len()
        ));
    };
    context
        .knowledge
        .has_type_role(target, TypeRole::Symbol)
        .then(|| {
            format!(
                "Installing a `<-` method on `{target}` has no effect: a symbol on the left of \
                 `<-` is assigned directly, before any method lookup, so `<-` can only be \
                 overloaded for types that are not symbols."
            )
        })
}

/// Not flexible derived from the operator spelling and its runtime form.
fn operator_not_flexible(context: &InstallationContext, sink: &mut DiagnosticSink) {
    let installation = context.installation;
    if installation.syntax == MethodInstallationSyntax::InstallMethod {
        return;
    }
    let MethodHead::Operator(operator) = &installation.method.head else {
        return;
    };
    if operator_form_is_flexible(operator, context.knowledge) != Some(false) {
        return;
    }
    sink.spawn(
        &catalog::OPERATOR_NOT_FLEXIBLE,
        Finding::new(
            installation.span,
            format!(
                "Cannot install a method on the {} operator `{}`: it is not flexible, so M2 rejects the assignment.",
                operator.form, operator.token
            ),
        ),
    );
}

/// Whether Macaulay2 lets users install methods on this form of the operator;
/// `None` when the operator is not in the catalog.
fn operator_form_is_flexible(
    operator: &Operator,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<bool> {
    knowledge
        .get_record(&operator.token)?
        .operator_info()
        .map(|operator_info| operator_info.is_flexible(operator.form))
}

/// Reports installation heads whose argument counts disagree with the implementation's lambda shape.
fn install_arity(context: &InstallationContext, sink: &mut DiagnosticSink) {
    let installation = context.installation;
    let expected = installation.expected_argument_arity();
    if matches!(
        installation.options,
        MethodOptions::Table | MethodOptions::TableOrForwarded
    ) {
        if let Some(Dispatch::Fixed(actual @ (0 | 2..))) = installation.installed_lambda_dispatch {
            let head = installation.method.head.name();
            sink.spawn(
                &catalog::INSTALL_ARITY,
                Finding::new(
                    installation.span,
                    format!(
                        "`{head}` takes options, so its installed function receives only the \
                         option table, not {actual} argument(s). Return the function of the \
                         arguments from it: `opts -> {} -> …`.",
                        parameter_list(expected)
                    ),
                ),
            );
            return;
        }
    }
    let Some(Dispatch::Fixed(actual)) = installation.argument_lambda_dispatch else {
        return;
    };
    if actual == expected {
        return;
    }
    sink.spawn(
        &catalog::INSTALL_ARITY,
        Finding::new(
            installation.span,
            format!(
                "This method's function takes {actual} argument(s) but the installation expects \
                 {expected}. Match the domain arity or use a variadic `x -> …`."
            ),
        ),
    );
}

/// Reports a method implementation whose option-table lambda shape does not match its callable.
fn install_option_form(context: &InstallationContext, sink: &mut DiagnosticSink) {
    let installation = context.installation;
    let head = installation.method.head.name();
    let arguments = parameter_list(installation.expected_argument_arity());
    let (rule, message) = match (installation.options, installation.installed) {
        (MethodOptions::Table, InstalledValue::OptionDeclaring) => (
            &catalog::INSTALL_OPTION_FORM,
            format!(
                "`{head}` passes its options as a table to the installed function, so a \
                 function declaring options with `>>` never receives them and the call fails. \
                 Install `o -> {arguments} -> …`."
            ),
        ),
        (MethodOptions::Table, InstalledValue::LibraryFunction { .. }) => (
            &catalog::INSTALL_OPTION_FORM,
            format!(
                "`{head}` calls the installed function with its option table and applies the \
                 result to the arguments, so a library function here makes the call fail. \
                 Install `o -> {arguments} -> …`."
            ),
        ),
        (MethodOptions::Forwarded, InstalledValue::Lambda)
        | (
            MethodOptions::Forwarded,
            InstalledValue::LibraryFunction {
                declares_options: false,
            },
        ) => (
            &catalog::INSTALL_OPTION_FORM,
            format!(
                "`{head}` forwards its options (`Options => true`), so the installed function \
                 must declare them; this one receives the option table as an extra argument. \
                 Install `{{…}} >> o -> {arguments} -> …`."
            ),
        ),
        (MethodOptions::Absent, InstalledValue::OptionDeclaring) => (
            &catalog::INSTALL_UNREACHABLE_OPTIONS,
            format!(
                "`{head}` has no options, so calls cannot pass any and the defaults declared \
                 with `>>` always apply. Install `{arguments} -> …`, or create `{head}` with \
                 `Options => true` to accept them."
            ),
        ),
        _ => return,
    };
    sink.spawn(rule, Finding::new(installation.span, message));
}

/// Placeholder parameters for an example lambda taking `arity` arguments.
fn parameter_list(arity: usize) -> String {
    match arity {
        1 => "x".to_string(),
        _ => format!(
            "({})",
            (1..=arity)
                .map(|index| format!("x{index}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
