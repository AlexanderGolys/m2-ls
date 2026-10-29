//! Shared, optional views over semantic graph nodes.
//!
//! Analysis owns the concrete records and their invariants. Features consume
//! these borrowed views instead of requiring a copied record per capability.

use tower_lsp::lsp_types::SymbolKind;

/// Whether a source binding is an ordinary name or a function parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingRole {
    /// A binding introduced by an ordinary declaration or assignment.
    Ordinary,
    /// A binding introduced by a function parameter.
    Parameter,
}

/// Presentation metadata exposed by bindings and other semantic objects.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    /// Editor symbol category when this item has one.
    pub symbol_kind: Option<SymbolKind>,
    /// Whether the item denotes a parameter or ordinary binding.
    pub binding_role: Option<BindingRole>,
    /// Human-readable inferred type, when one is available.
    pub type_label: Option<String>,
}

/// Provides the shared presentation facts consumed by editor features.
pub trait Metadata {
    /// Shared symbol and type metadata used by editor presentation.
    fn meta(&self) -> Meta;
}

impl Metadata for Meta {
    fn meta(&self) -> Meta {
        self.clone()
    }
}
