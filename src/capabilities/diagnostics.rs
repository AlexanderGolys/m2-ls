//! LSP publication of diagnostics produced by document analysis.

use tower_lsp::lsp_types::Url;
use tower_lsp::Client;

use crate::document::DocumentSnapshot;
use crate::settings::DiagnosticSettings;

/// The document's findings the user's settings allow, converted for publication.
pub fn visible_diagnostics(
    document: &DocumentSnapshot,
    settings: &DiagnosticSettings,
) -> Vec<tower_lsp::lsp_types::Diagnostic> {
    document
        .diagnostics()
        .iter()
        .filter(|diagnostic| settings.allows(diagnostic.kind))
        .map(|diagnostic| diagnostic.to_lsp())
        .collect()
}

/// Publishes the document's visible findings to the client, replacing any it
/// published for `uri` before.
pub async fn publish_diagnostics(
    client: &Client,
    uri: Url,
    document: &DocumentSnapshot,
    settings: &DiagnosticSettings,
) {
    client
        .publish_diagnostics(uri, visible_diagnostics(document, settings), None)
        .await;
}
