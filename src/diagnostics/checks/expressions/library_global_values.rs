//! What a rebindable library global may be assigned: a value of the class the
//! index records for it, within the range [`LibraryGlobals`] lists for it.
//! `allowableThreads = matrix {{1}}` and `allowableThreads = 0` both break
//! the setting, though Macaulay2 lets a program rebind the name.

use m2_syn::nodes::Expr;
use tower_lsp::lsp_types::Range as TextRange;

use super::ExpressionChecks;
use crate::analysis::{InferenceSession, KnownValue, TypeChecker};
use crate::builtin_index::Record;
use crate::diagnostics::{catalog, Finding};
use crate::library_globals::LibraryGlobals;
use crate::object_registry::ObjectRegistryView;
use crate::typesystem::{SubtypeEvidence, TypeRole};

/// Classes recording only that a global holds no value of its own yet: `null`,
/// as `lastMatch` before any match, or its own symbol, as `oo` before any
/// output. They say nothing of what the global is meant to hold.
const PLACEHOLDER_CLASSES: [TypeRole; 2] = [TypeRole::Nothing, TypeRole::Symbol];

impl<'a> ExpressionChecks<'a, '_> {
    /// Spawns when `value`, assigned at `range` to the rebindable library
    /// global `record`, has another class than the global holds, or is an
    /// integer outside the range the global accepts.
    pub fn library_global_value(
        &mut self,
        record: &Record,
        value: &'a Expr,
        range: TextRange,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let name = &record.name;
        let class = &record.class;
        if PLACEHOLDER_CLASSES
            .map(TypeRole::object_name)
            .contains(class)
        {
            return;
        }
        let analysis = self.context.analysis;
        let checker = TypeChecker::new(analysis, knowledge);
        let session = InferenceSession::new(&checker, self.context.source, self.snapshot);
        let scope = analysis.find_scope_at(range.start).unwrap_or(0);
        let message = if session
            .infer(value, scope)
            .possibility_by(class, |candidate, bound| {
                checker.subtype_evidence(candidate, bound, range.start, knowledge)
            })
            == SubtypeEvidence::Disproven
        {
            format!("`{name}` is a library setting holding a `{class}`; assign it a `{class}`")
        } else {
            let Some(accepted) = LibraryGlobals::get().integer_range(name) else {
                return;
            };
            let Some(KnownValue::Number(number)) = self.determined_value(value, knowledge) else {
                return;
            };
            match number.as_integer() {
                Some(integer) if !accepted.contains(integer) => {
                    format!("`{name}` accepts {accepted}, not {integer}")
                }
                _ => return,
            }
        };
        self.sink
            .spawn(&catalog::LIBRARY_GLOBAL_VALUE, Finding::new(range, message));
    }
}
