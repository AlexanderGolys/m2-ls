use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::ClientCapabilities;
use tower_lsp::Client;

pub trait ClientCapability {
    fn supported_by(capabilities: &ClientCapabilities) -> bool;
}

#[derive(Debug)]
pub struct ClientSupport<C> {
    supported: AtomicBool,
    capability: PhantomData<fn() -> C>,
}

impl<C> Default for ClientSupport<C> {
    fn default() -> Self {
        Self {
            supported: AtomicBool::new(false),
            capability: PhantomData,
        }
    }
}

impl<C: ClientCapability> ClientSupport<C> {
    pub fn negotiate(&self, capabilities: &ClientCapabilities) {
        self.supported
            .store(C::supported_by(capabilities), Ordering::Relaxed);
    }

    pub fn is_supported(&self) -> bool {
        self.supported.load(Ordering::Relaxed)
    }
}

pub trait WorkspaceRefresh<C> {
    fn refresh(&self) -> impl Future<Output = jsonrpc::Result<()>> + Send;
}

pub async fn refresh_if_changed<C>(
    support: &ClientSupport<C>,
    client: &(impl WorkspaceRefresh<C> + ?Sized),
    changed: bool,
) -> jsonrpc::Result<()>
where
    C: ClientCapability,
{
    if changed && support.is_supported() {
        client.refresh().await
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub struct InlayHintRefresh;

impl ClientCapability for InlayHintRefresh {
    fn supported_by(capabilities: &ClientCapabilities) -> bool {
        capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.inlay_hint.as_ref())
            .and_then(|inlay_hint| inlay_hint.refresh_support)
            .unwrap_or(false)
    }
}

impl WorkspaceRefresh<InlayHintRefresh> for Client {
    fn refresh(&self) -> impl Future<Output = jsonrpc::Result<()>> + Send {
        self.inlay_hint_refresh()
    }
}

#[derive(Debug)]
pub struct SemanticTokensRefresh;

impl ClientCapability for SemanticTokensRefresh {
    fn supported_by(capabilities: &ClientCapabilities) -> bool {
        capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.semantic_tokens.as_ref())
            .and_then(|semantic_tokens| semantic_tokens.refresh_support)
            .unwrap_or(false)
    }
}

impl WorkspaceRefresh<SemanticTokensRefresh> for Client {
    fn refresh(&self) -> impl Future<Output = jsonrpc::Result<()>> + Send {
        self.semantic_tokens_refresh()
    }
}

#[derive(Debug)]
pub struct SemanticTokensAugmentSyntax;

impl ClientCapability for SemanticTokensAugmentSyntax {
    fn supported_by(capabilities: &ClientCapabilities) -> bool {
        capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.semantic_tokens.as_ref())
            .and_then(|semantic_tokens| semantic_tokens.augments_syntax_tokens)
            .unwrap_or(false)
    }
}

#[derive(Debug)]
pub struct TypeHierarchyDynamicRegistration;

impl ClientCapability for TypeHierarchyDynamicRegistration {
    fn supported_by(capabilities: &ClientCapabilities) -> bool {
        capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.type_hierarchy)
            .and_then(|type_hierarchy| type_hierarchy.dynamic_registration)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct CountingRefresh {
        calls: AtomicUsize,
    }

    impl WorkspaceRefresh<InlayHintRefresh> for CountingRefresh {
        fn refresh(&self) -> impl Future<Output = jsonrpc::Result<()>> + Send {
            self.calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn refresh_requires_a_change_and_negotiated_support() {
        let support = ClientSupport::<InlayHintRefresh>::default();
        let client = CountingRefresh::default();

        refresh_if_changed(&support, &client, true).await.unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 0);

        support.negotiate(
            &serde_json::from_value(serde_json::json!({
                "workspace": {
                    "inlayHint": {
                        "refreshSupport": true
                    }
                }
            }))
            .unwrap(),
        );
        refresh_if_changed(&support, &client, false).await.unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 0);

        refresh_if_changed(&support, &client, true).await.unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn capability_markers_read_their_protocol_paths() {
        let capabilities = serde_json::from_value(serde_json::json!({
            "workspace": {
                "inlayHint": {
                    "refreshSupport": true
                },
                "semanticTokens": {
                    "refreshSupport": true
                }
            },
            "textDocument": {
                "semanticTokens": {
                    "requests": {},
                    "tokenTypes": [],
                    "tokenModifiers": [],
                    "formats": ["relative"],
                    "augmentsSyntaxTokens": true
                },
                "typeHierarchy": {
                    "dynamicRegistration": true
                }
            }
        }))
        .unwrap();

        assert!(InlayHintRefresh::supported_by(&capabilities));
        assert!(SemanticTokensRefresh::supported_by(&capabilities));
        assert!(SemanticTokensAugmentSyntax::supported_by(&capabilities));
        assert!(TypeHierarchyDynamicRegistration::supported_by(
            &capabilities
        ));
    }
}
