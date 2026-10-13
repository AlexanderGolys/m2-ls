//! Static type-hierarchy requests backed by generated builtin metadata.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use serde_json::{json, Value};
use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};
use tower_lsp::lsp_types::{Position, Range as TextRange, TypeHierarchyItem, Url};

use crate::builtin_index::Record;
use crate::document::DocumentSnapshot;
use crate::object_registry::ObjectName;
use crate::package_index::SourceResolver;
use crate::record_lsp::record_symbol_kind;
use crate::record_lsp::{LspKnowledge, PartitionedTypeKnowledge};
use crate::source::SourceNavigation;

pub const TYPE_HIERARCHY_METHOD: &str = "textDocument/prepareTypeHierarchy";

/// The static-metadata side of type hierarchy: resolve a record by name,
/// walk its parent/subtype edges, and materialize `TypeHierarchyItem`s. Born
/// out of the `Backend` handlers — kept here so `main.rs` only wires LSP
/// requests to thin shims, matching the other capability modules.
pub struct TypeHierarchyContext<'a, K: ?Sized> {
    knowledge: &'a K,
    source_resolver: &'a SourceResolver,
}

impl<'a, K: PartitionedTypeKnowledge + ?Sized> TypeHierarchyContext<'a, K> {
    pub fn new(knowledge: &'a K, source_resolver: &'a SourceResolver) -> Self {
        Self {
            knowledge,
            source_resolver,
        }
    }

    /// Prepare the type-hierarchy root for the symbol at `position`, when that
    /// symbol names a top-level type/class record in scope. The caller owns
    /// the document guard lookup and constructs the scoped knowledge view;
    /// everything past the snapshot lives here.
    pub fn prepare(
        &self,
        document: &DocumentSnapshot,
        scoped: &(impl LspKnowledge + ?Sized),
        uri: &Url,
        position: Position,
    ) -> Option<Vec<TypeHierarchyItem>> {
        let node = document.reference_node_at_position(position)?;
        let name = node.text();
        let range = document.range_for_node(node);

        let (package, record) = scoped.get_record_with_package(&ObjectName::new(name))?;
        record.type_info()?;

        Some(vec![self.item(
            &package,
            record,
            Some(uri.clone()),
            Some(range),
        )])
    }

    /// The parent type's item, if the originating item's record has one and we
    /// can resolve it (preferring the originating package, falling back to Core).
    /// A record with no resolvable parent yields an empty vec (rather than
    /// `None`, which the LSP treats as "no supertypes response at all").
    pub fn supertypes(&self, item: &TypeHierarchyItem) -> Option<Vec<TypeHierarchyItem>> {
        let package = Self::package_of(item);
        let (_, record) = self.record(package, &item.name)?;

        // Empty parent (no parent, self-parent, or unresolved edge) → empty
        // vec, not `None`.
        let resolved = (|| {
            let parent = record.type_info()?.parent.as_ref()?;
            if parent.object() == &record.id {
                return None;
            }
            let (parent_package, parent_record) = self.knowledge.get_type_by_id(parent)?;
            Some(self.item(&parent_package, parent_record, None, None))
        })();
        Some(resolved.into_iter().collect())
    }

    /// Every resolved subtype item of the originating item's record (skipping
    /// self-edges and unresolved names).
    pub fn subtypes(&self, item: &TypeHierarchyItem) -> Option<Vec<TypeHierarchyItem>> {
        let package = Self::package_of(item);
        let (_, record) = self.record(package, &item.name)?;

        let type_id = self.knowledge.type_id(&record.id)?;
        Some(
            self.knowledge
                .direct_subtypes(&type_id)
                .into_iter()
                .map(|(subtype_package, subtype_record)| {
                    self.item(&subtype_package, subtype_record, None, None)
                })
                .collect(),
        )
    }

    /// The originating package name stashed in the item's `data`, set when the
    /// item was first materialized (so follow-up supertypes/subtypes know which
    /// partition to resolve from before falling back to Core).
    fn package_of(item: &TypeHierarchyItem) -> Option<&str> {
        item.data
            .as_ref()
            .and_then(|data| data.get("package"))
            .and_then(|package| package.as_str())
    }

    /// Resolve the originating record itself, ensuring it carries type info (a
    /// non-type record has no hierarchy edges).
    fn record(&self, package: Option<&str>, name: &str) -> Option<(String, &Record)> {
        let package = package.unwrap_or("Core");
        let record = self
            .knowledge
            .get_record_from_package(package, &ObjectName::new(name))?;
        record.type_info()?;
        Some((package.to_string(), record))
    }

    /// Materialize a `TypeHierarchyItem` for `record`, filling in the
    /// occurrence URI/range when the request comes from an editor position, and
    /// the resolved source location otherwise (so a builtin type without an
    /// in-editor occurrence still has a navigable target).
    fn item(
        &self,
        package: &str,
        record: &Record,
        occurrence_uri: Option<Url>,
        occurrence_range: Option<TextRange>,
    ) -> TypeHierarchyItem {
        let location = self.source_resolver.package_location(package);
        let uri = occurrence_uri
            .or_else(|| location.as_ref().map(|location| location.uri.clone()))
            .unwrap_or_else(|| Url::parse("macaulay2:/builtins").expect("valid builtin URI"));
        let range = occurrence_range
            .or_else(|| location.as_ref().map(|location| location.range))
            .unwrap_or_else(|| TextRange::new(pos!(), pos!()));
        let detail = record
            .type_info()
            .and_then(|type_info| {
                type_info
                    .parent
                    .as_ref()
                    .filter(|parent| parent.object() != &record.id)
                    .and_then(|parent| self.knowledge.get_type_by_id(parent))
            })
            .map(|(_, parent)| format!("Parent: {}", parent.name));

        TypeHierarchyItem {
            name: record.name.0.clone(),
            kind: record_symbol_kind(record),
            tags: None,
            detail,
            uri,
            range,
            selection_range: range,
            data: Some(json!({
                "name": record.name.0.clone(),
                "package": package,
            })),
        }
    }
}

#[derive(Debug)]
pub struct TypeHierarchyCapabilityService<S> {
    inner: S,
}

impl<S> TypeHierarchyCapabilityService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> Service<Request> for TypeHierarchyCapabilityService<S>
where
    S: Service<Request, Response = Option<Response>> + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Option<Response>;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let should_advertise_type_hierarchy = req.method() == "initialize"
            && !request_type_hierarchy_dynamic_registration(req.params());
        let fut = self.inner.call(req);

        Box::pin(async move {
            let response = fut.await?;
            if should_advertise_type_hierarchy {
                Ok(response.map(advertise_type_hierarchy_capability))
            } else {
                Ok(response)
            }
        })
    }
}

/// Whether the client's `initialize` params advertise dynamic registration for
/// type hierarchy. When false (or absent), the server must declare the
/// capability statically in its `InitializeResult` instead.
pub fn request_type_hierarchy_dynamic_registration(params: Option<&Value>) -> bool {
    params
        .and_then(|params| params.get("capabilities"))
        .and_then(|capabilities| capabilities.get("textDocument"))
        .and_then(|text_document| text_document.get("typeHierarchy"))
        .and_then(|type_hierarchy| type_hierarchy.get("dynamicRegistration"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Inject `typeHierarchyProvider: true` into a successful `initialize` response.
/// Used when the client does not support dynamic registration, so the capability
/// is advertised statically (tower-lsp has no typed field for it).
pub fn advertise_type_hierarchy_capability(response: Response) -> Response {
    if !response.is_ok() {
        return response;
    }

    let (id, body) = response.into_parts();
    Response::from_parts(
        id,
        body.map(|mut result| {
            if let Some(capabilities) = result
                .get_mut("capabilities")
                .and_then(Value::as_object_mut)
            {
                capabilities
                    .entry("typeHierarchyProvider")
                    .or_insert_with(|| json!(true));
            }
            result
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn corpus() -> &'static str {
        include_str!("../data/m2-index.jsonl")
    }

    #[test]
    fn prepares_type_and_resolves_its_parent_from_partitioned_knowledge() {
        let knowledge = ObjectRegistry::load(corpus());
        let scoped = knowledge.with_source_imports("");
        let document = DocumentSnapshot::from_text("ZZ\n".to_string(), &knowledge)
            .expect("source should parse");
        let resolver = SourceResolver::new(Vec::new());
        let context = TypeHierarchyContext::new(&knowledge, &resolver);
        let uri = Url::parse("file:///type-hierarchy-test.m2").expect("valid test URI");

        let items = context
            .prepare(&document, &scoped, &uri, pos!())
            .expect("ZZ should be a known type");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "ZZ");

        let parents = context
            .supertypes(&items[0])
            .expect("known type should produce a hierarchy response");
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].name, "Number");
    }

    #[test]
    fn initialize_result_advertises_static_type_hierarchy() {
        let response = Response::from_ok(
            1.into(),
            json!({
                "capabilities": {
                    "hoverProvider": true
                }
            }),
        );

        let response = advertise_type_hierarchy_capability(response);
        let result = response
            .result()
            .expect("response should remain successful");

        assert_eq!(result["capabilities"]["typeHierarchyProvider"], json!(true));
    }

    #[test]
    fn type_hierarchy_dynamic_registration_detection_defaults_to_static() {
        let dynamic = Request::build("initialize")
            .params(json!({
                "capabilities": {
                    "textDocument": {
                        "typeHierarchy": {
                            "dynamicRegistration": true
                        }
                    }
                }
            }))
            .id(1)
            .finish();
        let static_only = Request::build("initialize")
            .params(json!({
                "capabilities": {
                    "textDocument": {
                        "typeHierarchy": {
                            "dynamicRegistration": false
                        }
                    }
                }
            }))
            .id(2)
            .finish();

        assert!(request_type_hierarchy_dynamic_registration(
            dynamic.params()
        ));
        assert!(!request_type_hierarchy_dynamic_registration(
            static_only.params()
        ));
        assert!(!request_type_hierarchy_dynamic_registration(None));
    }
}
