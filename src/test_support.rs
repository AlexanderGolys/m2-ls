//! Test-only semantic fixtures shared by crate tests.

use crate::analysis::Analysis;
use crate::builtin_index::Record;
use crate::node_metadata::SyntaxNode;
use crate::object_registry::{ObjectId, ObjectKnowledge, ObjectName, TypeId, TypeStore};
use crate::source::DocumentSource;
use crate::typesystem::{PositionedTypeKnowledge, TypeKnowledge};
use tower_lsp::lsp_types::Position;

/// Empty semantic knowledge for parser- and scope-only tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTypeKnowledge;

impl ObjectKnowledge for NoTypeKnowledge {
    fn object(&self, _object_id: &ObjectId) -> Option<&Record> {
        None
    }

    fn resolve_object(&self, _name: &ObjectName) -> Option<ObjectId> {
        None
    }
}

impl TypeStore for NoTypeKnowledge {
    fn parent_type_id(&self, _type_id: &TypeId) -> Option<TypeId> {
        None
    }

    fn has_strict_subtype_id(&self, _type_id: &TypeId) -> bool {
        false
    }
}

impl TypeKnowledge for NoTypeKnowledge {}

impl PositionedTypeKnowledge for NoTypeKnowledge {
    type Knowledge<'a> = Self;

    fn at_position(&self, _position: Position) -> Self::Knowledge<'_> {
        *self
    }
}

/// Analyze one syntax fixture without external object knowledge.
pub fn analyze(root: SyntaxNode<'_>) -> Analysis {
    let source = DocumentSource::new(root.text().to_string());
    Analysis::new_with_knowledge(root, None, &source, &NoTypeKnowledge)
}
