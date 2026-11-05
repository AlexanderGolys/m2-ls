//! Common object hierarchies observed through framed language-server requests.

use crate::support::{position, response_array, DocumentSession, LspProcess, TestWorkspace};
use serde_json::json;

/// Shared hierarchy reports retain object, class, inheritance, and dictionary identities.
#[tokio::test]
async fn workspace_hierarchies_include_source_and_library_objects() {
    let source = concat!(
        "newPackage \"HierarchyExample\"\n",
        "Meta = new Type of SelfInitializingType;\n",
        "Entry = new Meta of BasicList;\n",
        "new Entry from Thing := (t,x) -> {x};\n",
        "entry = Entry \"value\";\n",
        "helper = x -> x;\n",
        "export {\"Entry\", \"Alias\" => \"Entry\"}\n",
        "endPackage \"HierarchyExample\"\n",
        "List\n",
    );
    let mut session =
        DocumentSession::open_with_related(source, "Entry = new Type of HashTable;\n").await;
    let report = session
        .request(
            "workspace/executeCommand",
            json!({"command": "m2-ls.printHierarchies", "arguments": []}),
        )
        .await;
    let objects = report["hierarchy"]["objects"]
        .as_object()
        .expect("hierarchy object map");
    let source_object = |name: &str| {
        objects
            .values()
            .find(|object| {
                object["name"] == name
                    && object["location"]["uri"] == session.uri()
                    && object["scope"] == 0
            })
            .unwrap_or_else(|| panic!("missing source object {name}: {report}"))
    };
    let entry = source_object("Entry");
    let meta = source_object("Meta");
    assert_eq!(entry["classOf"], meta["id"]);
    assert_eq!(entry["parentOf"], "$Core$BasicList");
    assert_eq!(meta["classOf"], "$Core$Type");
    assert_eq!(meta["parentOf"], "$Core$SelfInitializingType");
    assert_eq!(source_object("entry")["classOf"], entry["id"]);
    assert_eq!(source_object("helper")["classOf"], "$Core$FunctionClosure");
    assert_eq!(
        source_object("HierarchyExample")["classOf"],
        "$Core$Package"
    );
    assert_eq!(objects["$Core$List"]["classOf"], "$Core$Type");
    assert_eq!(objects["$Core$List"]["parentOf"], "$Core$VisibleList");
    assert_eq!(objects["$Core$Core"]["classOf"], "$Core$Package");
    let same_named = objects
        .values()
        .filter(|object| object["name"] == "Entry")
        .collect::<Vec<_>>();
    assert_eq!(
        same_named.len(),
        2,
        "source identities must not collide between documents"
    );
    assert_ne!(same_named[0]["id"], same_named[1]["id"]);
    let namespaces = response_array(&report["hierarchy"]["namespaces"]);
    let public = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "HierarchyExample")
        .expect("export dictionary");
    assert_eq!(public["bindings"]["Entry"], entry["id"]);
    assert_eq!(public["bindings"]["Alias"], entry["id"]);
    assert!(public["bindings"].get("helper").is_none());
    let private = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "HierarchyExample (private)")
        .expect("private dictionary");
    assert_eq!(private["bindings"]["helper"], source_object("helper")["id"]);
    let packages = namespaces
        .iter()
        .find(|namespace| namespace["id"]["kind"] == "packages")
        .expect("package-name dictionary");
    assert_eq!(
        packages["bindings"]["HierarchyExample"],
        source_object("HierarchyExample")["id"]
    );
    assert_eq!(packages["bindings"]["Core"], "$Core$Core");
    assert!(report["text"]
        .as_str()
        .unwrap()
        .contains("parentOf (inheritance"));
    assert!(report["text"]
        .as_str()
        .unwrap()
        .contains("classOf (runtime class"));
    session.shutdown().await;
}

/// The standard type hierarchy traverses one shared graph across source and library edges.
#[tokio::test]
async fn type_hierarchy_follows_parent_edges_across_object_origins() {
    let source = concat!(
        "Meta = new Type of SelfInitializingType;\n",
        "Entry = new Meta of BasicList;\n",
        "new Entry from Thing := (t,x) -> {x};\n",
        "entry = Entry \"value\";\n",
        "Entry\n",
        "BasicList\n",
        "entry\n",
    );
    let mut session = DocumentSession::open(source).await;
    let roots = session
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({"textDocument":{"uri":session.uri()}, "position":{"line":4,"character":0}}),
        )
        .await;
    let entry = response_array(&roots)
        .first()
        .expect("source-defined type hierarchy")
        .clone();
    assert_eq!(entry["name"], "Entry");
    assert!(entry["detail"].as_str().unwrap().contains("classOf: Meta"));
    let parents = session
        .request("typeHierarchy/supertypes", json!({"item":entry.clone()}))
        .await;
    let parent = response_array(&parents)
        .first()
        .expect("library parent of source type")
        .clone();
    assert_eq!(
        parent["name"], "BasicList",
        "classOf must never be used as parentOf"
    );
    let children = session
        .request("typeHierarchy/subtypes", json!({"item":parent}))
        .await;
    assert!(
        response_array(&children)
            .iter()
            .any(|child| child["data"] == entry["data"]),
        "library parent must find a source child: {children}"
    );
    let instance = session
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({"textDocument":{"uri":session.uri()}, "position":{"line":6,"character":0}}),
        )
        .await;
    assert_eq!(response_array(&instance)[0]["data"], entry["data"]);
    let class_parents = session.request("typeHierarchy/supertypes", json!({"item":children.as_array().unwrap().iter().find(|child| child["name"] == "VisibleList").unwrap()})).await;
    assert_eq!(response_array(&class_parents)[0]["name"], "BasicList");
    session.shutdown().await;
}

/// Qualified class references survive another package defining the same class name.
#[tokio::test]
async fn class_relations_preserve_package_identity_and_command_advertising() {
    let workspace = TestWorkspace::new("");
    let corpus = [
        json!({"kind":"meta", "default_loaded":["Core","First","Second"]}),
        json!({"kind":"type", "name":"Thing", "package":"$Core$Core", "class":"$Core$Type"}),
        json!({"kind":"type", "name":"Type", "package":"$Core$Core", "class":"$Core$Type", "parent":"$Core$Thing"}),
        json!({"kind":"type", "name":"Package", "package":"$Core$Core", "class":"$Core$Type", "parent":"$Core$Thing"}),
        json!({"kind":"type", "name":"Meta", "package":"$First$First", "class":"$Core$Type", "parent":"$Core$Thing"}),
        json!({"kind":"type", "name":"Meta", "package":"$Second$Second", "class":"$Core$Type", "parent":"$Core$Thing"}),
        json!({"kind":"package", "name":"First", "package":"$First$First", "class":"$Core$Package", "package_exports":["Second"]}),
        json!({"kind":"package", "name":"Second", "package":"$Second$Second", "class":"$Core$Package", "package_exports":["First"]}),
        json!({"kind":"object", "name":"firstObject", "package":"$First$First", "class":"$First$Meta"}),
        json!({"kind":"object", "name":"secondObject", "package":"$Second$Second", "class":"$Second$Meta"}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    workspace.write_file("catalog.jsonl", &corpus);
    let mut server = LspProcess::spawn_with_catalog(Some(&workspace.path("catalog.jsonl"))).await;
    let initialized = server.initialize(&workspace.root_uri()).await;
    assert!(
        response_array(&initialized["capabilities"]["executeCommandProvider"]["commands"])
            .iter()
            .any(|command| command == "m2-ls.printHierarchies")
    );
    let report = server
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let objects = &report["hierarchy"]["objects"];
    assert_eq!(objects["$First$firstObject"]["classOf"], "$First$Meta");
    assert_eq!(objects["$Second$secondObject"]["classOf"], "$Second$Meta");
    assert_eq!(objects["$Core$Type"]["classOf"], "$Core$Type");
    assert_eq!(objects["$Core$Type"]["parentOf"], "$Core$Thing");
    assert!(response_array(&report["hierarchy"]["packageExports"])
        .iter()
        .any(|edge| edge["package"] == "$First$First" && edge["dependency"] == "$Second$Second"));
    assert!(report["text"]
        .as_str()
        .unwrap()
        .contains("public namespaces: First -> Second"));
    server.shutdown().await;
}

/// Aliases bind another name to the same object for user and library values alike.
#[tokio::test]
async fn namespace_aliases_preserve_types_packages_and_ordinary_objects() {
    let source = concat!(
        "newPackage \"AliasExample\"\n",
        "Entry = new SelfInitializingType of BasicList;\n",
        "EntryAlias = Entry;\n",
        "entry = Entry {};\n",
        "entryAlias = entry;\n",
        "ListAlias = List;\n",
        "PackageAlias = Core;\n",
        "export {\"EntryAlias\", \"ListAlias\"}\n",
        "endPackage \"AliasExample\"\n",
        "EntryAlias\n",
        "ListAlias\n",
    );
    let mut session = DocumentSession::open(source).await;
    let report = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let namespaces = response_array(&report["hierarchy"]["namespaces"]);
    let private = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "AliasExample (private)")
        .unwrap();
    let public = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "AliasExample")
        .unwrap();
    assert_eq!(
        private["bindings"]["EntryAlias"],
        private["bindings"]["Entry"]
    );
    assert_eq!(
        private["bindings"]["entryAlias"],
        private["bindings"]["entry"]
    );
    assert_eq!(private["bindings"]["ListAlias"], "$Core$List");
    assert_eq!(private["bindings"]["PackageAlias"], "$Core$Core");
    assert_eq!(
        public["bindings"]["EntryAlias"],
        private["bindings"]["Entry"]
    );
    assert_eq!(public["bindings"]["ListAlias"], "$Core$List");
    let aliases = session
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({"textDocument":{"uri":session.uri()},"position":{"line":9,"character":0}}),
        )
        .await;
    assert_eq!(response_array(&aliases)[0]["name"], "Entry");
    let library_alias = session
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({"textDocument":{"uri":session.uri()},"position":{"line":10,"character":0}}),
        )
        .await;
    assert_eq!(response_array(&library_alias)[0]["name"], "List");
    session.shutdown().await;
}

/// Unopened package files share their package's namespace and export identities.
#[tokio::test]
async fn loaded_package_files_contribute_objects_and_exports() {
    let source = concat!(
        "newPackage \"LoadedHierarchy\"\n",
        "load \"part.m2\"\n",
        "export {\"LoadedType\"}\n",
        "endPackage \"LoadedHierarchy\"\n",
        "LoadedType\n",
    );
    let workspace = TestWorkspace::new(source);
    let loaded_uri = workspace.write_file("part.m2", "LoadedType = new Type of BasicList;\n");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;
    let report = server
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let objects = report["hierarchy"]["objects"].as_object().unwrap();
    let loaded = objects
        .values()
        .find(|object| object["name"] == "LoadedType" && object["location"]["uri"] == loaded_uri)
        .expect("closed loaded file must be represented");
    assert_eq!(loaded["classOf"], "$Core$Type");
    assert_eq!(loaded["parentOf"], "$Core$BasicList");
    let public = response_array(&report["hierarchy"]["namespaces"])
        .iter()
        .find(|namespace| namespace["name"] == "LoadedHierarchy")
        .expect("public package dictionary");
    assert_eq!(public["bindings"]["LoadedType"], loaded["id"]);
    let roots = server
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({"textDocument":{"uri":workspace.uri},"position":{"line":4,"character":0}}),
        )
        .await;
    assert_eq!(response_array(&roots)[0]["name"], "LoadedType");
    server.shutdown().await;
}

/// Empty outer lexical dictionaries remain present and edits replace their prior object graph.
#[tokio::test]
async fn hierarchy_snapshots_keep_dictionary_links_and_follow_edits() {
    let source = concat!(
        "f = () -> (if true then a := 1; () -> b := 2);\n",
        "g = () -> (() -> c := 3);\n",
        "Entry = new Type of BasicList;\n",
    );
    let mut session = DocumentSession::open(source).await;
    let report = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let repeated = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    assert_eq!(report, repeated, "inspection must not mutate the hierarchy");
    let namespaces = response_array(&report["hierarchy"]["namespaces"]);
    for namespace in namespaces {
        if !namespace["enclosing"].is_null() {
            assert!(
                namespaces
                    .iter()
                    .any(|parent| parent["id"] == namespace["enclosing"]),
                "dangling enclosing dictionary: {namespace}"
            );
        }
    }
    let objects = report["hierarchy"]["objects"].as_object().unwrap();
    for object in objects.values() {
        for relation in ["classOf", "parentOf"] {
            if let Some(id) = object[relation].as_str() {
                assert!(
                    objects.contains_key(id),
                    "dangling {relation} edge: {object}"
                );
            }
        }
    }
    let previous = objects
        .values()
        .find(|object| object["name"] == "Entry")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    session
        .replace("Replacement = new Type of HashTable;\n")
        .await;
    let current = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    assert!(current["hierarchy"]["objects"].get(&previous).is_none());
    assert!(current["hierarchy"]["objects"]
        .as_object()
        .unwrap()
        .values()
        .any(|object| object["name"] == "Replacement" && object["parentOf"] == "$Core$HashTable"));
    session.shutdown().await;
}

/// Qualified source names distinguish package objects, their class, and User globals.
#[tokio::test]
async fn namespace_qualifiers_follow_macaulay2_package_dictionaries() {
    let source = "globalValue = 3;\nunresolvedUser = User$User;\nuserPackage = User;\nnewPackage \"QualifiedExample\"\npublicValue = 7;\nprivateValue = 9;\nexport {\"publicValue\"}\nendPackage \"QualifiedExample\"\nCore$Package\nPackage$Core\nPackage$QualifiedExample\nQualifiedExample$publicValue\nUser$globalValue\nUser\nCore$Core\n";
    let mut session = DocumentSession::open(source).await;
    let report = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let objects = report["hierarchy"]["objects"].as_object().unwrap();
    let namespaces = response_array(&report["hierarchy"]["namespaces"]);
    let packages = namespaces
        .iter()
        .find(|namespace| namespace["id"]["kind"] == "packages")
        .unwrap();
    assert_eq!(packages["qualifiedBindings"]["Package$Core"], "$Core$Core");
    assert!(packages["qualifiedBindings"].get("Core$Core").is_none());
    assert!(packages["qualifiedBindings"]
        .get("Package$Package")
        .is_none());
    assert!(packages["qualifiedBindings"].get("Package$User").is_none());
    assert!(packages["qualifiedBindings"]["Package$QualifiedExample"].is_string());
    let core = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "Core")
        .unwrap();
    assert_eq!(core["qualifiedBindings"]["Core$Package"], "$Core$Package");
    assert!(core["qualifiedBindings"].get("Core$Core").is_none());
    let user = namespaces
        .iter()
        .find(|namespace| namespace["id"]["kind"] == "document")
        .unwrap();
    assert_eq!(
        user["qualifiedBindings"]["User$globalValue"],
        user["bindings"]["globalValue"]
    );
    assert!(user["qualifiedBindings"].get("User$User").is_none());
    let owner = user["ownerPackage"].as_str().unwrap();
    assert_eq!(objects[owner]["name"], "User");
    assert_eq!(objects[owner]["classOf"], "$Core$Package");
    assert_eq!(objects[owner]["package"], owner);
    assert_eq!(objects["$Core$Core"]["package"], "$Core$Core");
    assert_eq!(objects["$Core$Package"]["package"], "$Core$Core");
    assert_eq!(user["bindings"]["userPackage"], owner);
    assert_eq!(
        objects[user["bindings"]["unresolvedUser"].as_str().unwrap()]["classOf"],
        "$Core$Symbol"
    );
    let public = namespaces
        .iter()
        .find(|namespace| namespace["name"] == "QualifiedExample")
        .unwrap();
    assert!(public["qualifiedBindings"]["QualifiedExample$publicValue"].is_string());
    assert!(public["qualifiedBindings"]
        .get("QualifiedExample$privateValue")
        .is_none());
    for (name, expected) in [
        ("Core$Package", "Package"),
        ("Package$Core", "Package"),
        ("Package$QualifiedExample", "Package"),
        ("QualifiedExample$publicValue", "ZZ"),
        ("User$globalValue", "ZZ"),
    ] {
        let roots = session
            .request(
                "textDocument/prepareTypeHierarchy",
                json!({
                    "textDocument":{"uri":session.uri()}, "position": position(source, name, 0)
                }),
            )
            .await;
        assert_eq!(
            response_array(&roots)[0]["name"],
            expected,
            "{name}: {roots}"
        );
    }
    let invalid = session
        .request(
            "textDocument/prepareTypeHierarchy",
            json!({
                "textDocument":{"uri":session.uri()}, "position":position(source,"Core$Core",0)
            }),
        )
        .await;
    assert!(
        response_array(&invalid).is_empty(),
        "a fabricated package alias must not resolve: {invalid}"
    );
    session.shutdown().await;
}

/// Public package dependencies autoload the dependency's dictionary without copying its names.
#[tokio::test]
async fn package_exports_link_forwarded_public_namespaces() {
    let source = "newPackage(\"ForwardedHierarchy\", PackageExports => {\"JSON\"})\nendPackage \"ForwardedHierarchy\"\n";
    let mut session = DocumentSession::open(source).await;
    let report = session
        .request(
            "workspace/executeCommand",
            json!({"command":"m2-ls.printHierarchies"}),
        )
        .await;
    let objects = report["hierarchy"]["objects"].as_object().unwrap();
    let package = objects
        .values()
        .find(|object| object["name"] == "ForwardedHierarchy")
        .unwrap();
    let json_package = objects
        .values()
        .find(|object| object["name"] == "JSON" && object["namespace"]["kind"] == "packages")
        .unwrap();
    let edge = response_array(&report["hierarchy"]["packageExports"])
        .iter()
        .find(|edge| edge["package"] == package["id"])
        .expect("forwarded dependency edge");
    assert_eq!(edge["dependency"], json_package["id"]);
    assert_eq!(edge["name"], "JSON");
    let json = response_array(&report["hierarchy"]["namespaces"])
        .iter()
        .find(|namespace| namespace["name"] == "JSON")
        .unwrap();
    assert!(json["qualifiedBindings"]["JSON$toJSON"].is_string());
    let public = response_array(&report["hierarchy"]["namespaces"])
        .iter()
        .find(|namespace| namespace["name"] == "ForwardedHierarchy")
        .unwrap();
    assert!(public["qualifiedBindings"]
        .get("ForwardedHierarchy$toJSON")
        .is_none());
    assert!(report["text"]
        .as_str()
        .unwrap()
        .contains("public namespaces: ForwardedHierarchy -> JSON"));
    session.shutdown().await;
}
