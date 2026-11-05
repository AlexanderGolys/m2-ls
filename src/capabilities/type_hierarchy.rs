//! Static type-hierarchy requests backed by generated builtin metadata.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use serde_json::{json, Value};
use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};
use tower_lsp::lsp_types::{Position, Range as TextRange, TypeHierarchyItem, Url};

use crate::document::DocumentSnapshot;
use crate::object_hierarchy::{HierarchyObject, NamespaceId, ObjectHierarchy};
use crate::object_registry::{ObjectId, ObjectKnowledge, ObjectName};
use crate::package_index::SourceResolver;
use crate::source::SourceNavigation;
use crate::workspace_index::WorkspaceDefinitionKnowledge;
use tower_lsp::lsp_types::SymbolKind;

/// Protocol request name intercepted for dynamic type-hierarchy support.
pub const TYPE_HIERARCHY_METHOD: &str = "textDocument/prepareTypeHierarchy";

/// Type hierarchy over shared object identities and their inheritance edges.
/// Source locations and catalog metadata affect presentation, not traversal.
pub struct TypeHierarchyContext<'a> {
    /// Common graph including objects from the surrounding workspace and packages.
    hierarchy: &'a ObjectHierarchy,
    /// Resolves source locations for objects supplied by installed packages.
    source_resolver: &'a SourceResolver,
}

impl<'a> TypeHierarchyContext<'a> {
    /// Uses the shared object graph for every prepare and traversal request.
    pub fn new(hierarchy: &'a ObjectHierarchy, source_resolver: &'a SourceResolver) -> Self {
        Self {
            hierarchy,
            source_resolver,
        }
    }

    /// Resolves the object under the cursor, then prepares its type hierarchy.
    /// A type object selects itself; other objects select their runtime class.
    pub fn prepare(
        &self,
        document: &DocumentSnapshot,
        uri: &Url,
        position: Position,
        workspace: &(impl WorkspaceDefinitionKnowledge + ?Sized),
    ) -> Option<Vec<TypeHierarchyItem>> {
        let symbol = document.reference_symbol_at(position)?.symbol;
        let name = symbol.text();
        let range = document.range_for_syntax(symbol)?;
        let source = document
            .binding_at_position(position)
            .or_else(|| document.source_binding_at(name, position));
        let objects = if let Some(binding) = source {
            vec![document.analysis().binding_object_id(binding)]
        } else {
            let locations = workspace.definitions(name, uri);
            let objects = self
                .hierarchy
                .objects
                .values()
                .filter(|object| {
                    object
                        .location
                        .as_ref()
                        .is_some_and(|location| locations.contains(location))
                })
                .map(|object| object.id.clone())
                .collect::<Vec<_>>();
            if objects.is_empty() {
                let qualified = self.hierarchy.resolve_qualified(name, uri);
                if qualified.is_empty() {
                    document
                        .object_registry()
                        .at(position)
                        .resolve_object(&ObjectName::new(name))
                        .into_iter()
                        .collect()
                } else {
                    qualified
                }
            } else {
                objects
            }
        };
        let mut seen = std::collections::HashSet::new();
        Some(
            objects
                .into_iter()
                .filter_map(|id| {
                    let object = self.hierarchy.objects.get(&id)?;
                    let selected = object.type_id.as_ref().or(object.class_of.as_ref())?;
                    if !seen.insert(selected.clone()) {
                        return None;
                    }
                    let selected = self.hierarchy.objects.get(selected.object())?;
                    let mut item = self.item(selected);
                    item.uri = uri.clone();
                    item.range = range;
                    item.selection_range = range;
                    Some(item)
                })
                .collect(),
        )
    }

    /// The direct inheritance parent, independent of the selected type's class.
    pub fn supertypes(&self, item: &TypeHierarchyItem) -> Option<Vec<TypeHierarchyItem>> {
        let object = self.object(item)?;
        Some(
            object
                .parent_of
                .as_ref()
                .filter(|parent| parent.object() != &object.id)
                .and_then(|parent| self.hierarchy.objects.get(parent.object()))
                .map(|parent| self.item(parent))
                .into_iter()
                .collect(),
        )
    }

    /// All direct inheritance children, including edges across object origins.
    pub fn subtypes(&self, item: &TypeHierarchyItem) -> Option<Vec<TypeHierarchyItem>> {
        let object = self.object(item)?;
        let type_id = object.type_id.as_ref()?;
        Some(
            self.hierarchy
                .subtypes(type_id)
                .into_iter()
                .map(|child| self.item(child))
                .collect(),
        )
    }

    /// Resolves the canonical identity retained in a hierarchy item.
    fn object(&self, item: &TypeHierarchyItem) -> Option<&HierarchyObject> {
        let id = item.data.as_ref()?.get("objectId")?.as_str()?;
        self.hierarchy.objects.get(&ObjectId::new(id))
    }

    /// Presents one type identically whether source or catalog supplied its facts.
    fn item(&self, object: &HierarchyObject) -> TypeHierarchyItem {
        let package = match &object.namespace {
            NamespaceId::Package(_) | NamespaceId::PackagePrivate(_) => self
                .hierarchy
                .namespaces
                .iter()
                .find(|namespace| namespace.id == object.namespace)
                .map(|namespace| namespace.name.as_str()),
            _ => None,
        };
        let location = object
            .location
            .clone()
            .or_else(|| package.and_then(|package| self.source_resolver.package_location(package)));
        let range = location
            .as_ref()
            .map(|location| location.range)
            .unwrap_or_else(|| TextRange::new(pos!(), pos!()));
        let uri = location
            .map(|location| location.uri)
            .unwrap_or_else(|| Url::parse("macaulay2:/builtins").expect("valid builtin URI"));
        let class = object
            .class_of
            .as_ref()
            .and_then(|class| self.hierarchy.objects.get(class.object()))
            .map(|class| class.name.as_str())
            .unwrap_or("unknown");
        let parent = object
            .parent_of
            .as_ref()
            .and_then(|parent| self.hierarchy.objects.get(parent.object()))
            .map(|parent| parent.name.as_str())
            .unwrap_or("unknown");
        TypeHierarchyItem {
            name: object.name.clone(),
            kind: SymbolKind::CLASS,
            tags: None,
            detail: Some(format!("classOf: {class}; parentOf: {parent}")),
            uri,
            range,
            selection_range: range,
            data: Some(json!({"objectId": object.id.name()})),
        }
    }
}

/// Service wrapper recognizing dynamically enabled type-hierarchy requests.
#[derive(Debug)]
pub struct TypeHierarchyCapabilityService<S> {
    /// Wrapped service receiving requests after interception.
    inner: S,
}

impl<S> TypeHierarchyCapabilityService<S> {
    /// Constructs this value from the supplied dependencies and initial state.
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
        crate::catalog::test_corpus()
    }

    #[test]
    fn prepares_type_and_resolves_its_parent_from_partitioned_knowledge() {
        let knowledge = ObjectRegistry::load(corpus());
        let mut hierarchy = ObjectHierarchy::default();
        hierarchy.include_environment(&knowledge, pos_max!());
        let document = DocumentSnapshot::from_text("ZZ\n".to_string(), &knowledge)
            .expect("source should parse");
        let resolver = SourceResolver::default();
        let context = TypeHierarchyContext::new(&hierarchy, &resolver);
        let uri = Url::parse("file:///type-hierarchy-test.m2").expect("valid test URI");

        let items = context
            .prepare(
                &document,
                &uri,
                pos!(),
                &crate::workspace_index::WorkspaceIndex::default(),
            )
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
