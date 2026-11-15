//! The orphan rule: a package should install a method only where it owns the
//! method function or one of the types the method dispatches on.
//!
//! Macaulay2 looks a method up by the classes of the actual arguments: for
//! `f(x, y)` it tries the key `(f, X, Y)` for every pair of ancestors `X` of
//! `class x` and `Y` of `class y`, up to `Thing` (`lookupBinaryMethod` in M2's
//! `d/hashtables.dd`, and likewise for one and three arguments). An
//! installation is therefore reached only by arguments descending from each of
//! its domain types. When the package defines one of those types, only values
//! of that type, which the package itself creates, reach the method; when it
//! defines the method function, only callers of that function do. With neither,
//! loading the package changes what `f` does for every session, including code
//! that uses nothing from the package.
//!
//! Ownership follows the value, not the name: `T = ZZ; f T := …` installs on
//! `ZZ`. A codomain annotation is not dispatched on and owns nothing. A name
//! whose value is unknown, such as one defined by a file the package loads that
//! is not indexed, counts as owned, so the rule reports only installations it
//! can prove foreign.

use tower_lsp::lsp_types::Position;

use super::{DiagnosticSink, InstallationContext};
use crate::analysis::MethodHead;
use crate::diagnostics::{catalog, Finding};
use crate::object_registry::{ObjectKnowledge, ObjectName};

/// Who defines a name the installation dispatches through.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ownership {
    /// The installing package, or a value the source creates.
    Owned,
    /// Another package, named, such as `Core`.
    Foreign(String),
}

/// Warns when the installation, made by code of the packages `owners`, owns
/// neither its method function nor any of its domain types.
pub(super) fn spawn(
    context: &InstallationContext,
    owners: &[ObjectName],
    sink: &mut DiagnosticSink,
) {
    let installation = context.installation;
    let method = &installation.method;
    let Some(package) = owners.first() else {
        return;
    };
    let at = installation.span.start;
    let head = match &method.head {
        MethodHead::Function(name) => match ownership(context, name, at, owners) {
            Ownership::Owned => return,
            Ownership::Foreign(owner) => Some((name, owner)),
        },
        // Operators belong to the language, never to a package.
        MethodHead::Operator(_) => None,
    };
    let mut domain = Vec::with_capacity(method.domain.len());
    for name in &method.domain {
        match ownership(context, name, at, owners) {
            Ownership::Owned => return,
            Ownership::Foreign(owner) => domain.push((name, owner)),
        }
    }
    let described = |(name, owner): &(&ObjectName, String)| format!("`{name}` ({owner})");
    let head_text = match &head {
        Some(head) => described(head),
        None => format!("`{}`", method.head.name()),
    };
    let plain = method.head.name();
    let message = if domain.is_empty() {
        format!(
            "`{package}` installs a method on {head_text} without defining it, so loading \
             `{package}` changes `{plain}` for every session, even code that uses nothing from \
             it. Install it on a method function of your own."
        )
    } else {
        let types = domain.iter().map(described).collect::<Vec<_>>().join(", ");
        format!(
            "`{package}` installs {head_text} for {types} without defining the method or any \
             of these types, so loading `{package}` changes `{plain}` for every session, even \
             code that uses nothing from it. Install it on a type or method function of your \
             own."
        )
    };
    let mut finding = Finding::new(installation.span, message);
    let names = head
        .iter()
        .map(|(name, _)| *name)
        .chain(domain.iter().map(|(name, _)| *name));
    for name in names {
        if let Some(binding) = context.analysis.get_binding_at(name.name(), at) {
            finding = finding.with_note(
                binding.binding.range,
                format!("`{name}` is bound here to a value another package defines"),
            );
        }
    }
    sink.spawn(&catalog::ORPHAN_INSTALLATION, finding);
}

/// Who defines the value `name` has at `at`. A value created by the source is
/// owned whether or not it is exported; a name nothing resolves, or a binding
/// whose value is unknown, counts as owned too.
fn ownership(
    context: &InstallationContext,
    name: &ObjectName,
    at: Position,
    owners: &[ObjectName],
) -> Ownership {
    let knowledge = context.knowledge;
    let identity = match context.analysis.get_binding_at(name.name(), at) {
        Some(binding) => match &binding.state.value_identity {
            Some(identity) => identity.clone(),
            None => return Ownership::Owned,
        },
        None => match knowledge.resolve_object(name) {
            Some(identity) => identity,
            None => return Ownership::Owned,
        },
    };
    if knowledge.is_source_object(&identity) {
        return Ownership::Owned;
    }
    let Some(package) = knowledge
        .object(&identity)
        .and_then(|record| knowledge.package_name(&record.package))
    else {
        return Ownership::Owned;
    };
    if owners.iter().any(|owner| owner.name() == package) {
        Ownership::Owned
    } else {
        Ownership::Foreign(package.to_string())
    }
}
