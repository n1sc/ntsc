use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

struct Project {
    directory: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("ntsc_lsp_{name}_{nonce}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("neutron.toml"),
            "[package]\nentry = \"main.nt\"\noutput = \"test\"\n",
        )
        .unwrap();
        Self { directory }
    }

    fn file(&self, name: &str, source: &str) -> PathBuf {
        let path = self.directory.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!(
                "cannot remove test project {}: {error}",
                self.directory.display()
            );
        }
    }
}

fn open(server: &mut Server<'_>, path: &Path, source: &str) -> (Url, usize) {
    let offset = source.find("$CURSOR").unwrap_or(source.len());
    let text = source.replace("$CURSOR", "");
    fs::write(path, &text).unwrap();
    let uri = Url::from_file_path(path).unwrap();
    server
        .documents
        .insert(uri.to_string(), Document::new(uri.clone(), 1, text));
    server.refresh_workspace_files();
    (uri, offset)
}

fn completions(server: &Server<'_>, uri: &Url, offset: usize) -> Vec<CompletionItem> {
    let document = server.document_for(uri).unwrap();
    let response = server
        .completion(lsp_types::CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                position: document.index.position(&document.text, offset),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .unwrap();
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

#[test]
fn members_follow_inferred_factories_and_chained_receivers() {
    let project = Project::new("receiver");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let source = r#"
class Child { var int count; fun ping() -> int { return 1; } }
class Other { var string unrelated; fun unrelated_method() {} }
class Parent { var Child child; }
fun make() -> Parent { return Parent(); }
fun main() { make().child.$CURSOR }
"#;
    let (uri, offset) = open(&mut server, &path, source);
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"count".to_string()), "{labels:?}");
    assert!(labels.contains(&"ping".to_string()), "{labels:?}");
    assert!(!labels.contains(&"unrelated".to_string()), "{labels:?}");
    assert!(!labels.contains(&"child".to_string()), "{labels:?}");
}

#[test]
fn unknown_receivers_and_unawaited_futures_do_not_guess_members() {
    let project = Project::new("unknown");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    for source in [
        "class Item { var int count; } fun main(any value) { value.$CURSOR }",
        "class Item { var int count; } async fun make() -> Item { return Item(); } fun main() { var pending = make(); pending.$CURSOR }",
        "class Item { var int count; } fun main() { Item.$CURSOR }",
    ] {
        let (uri, offset) = open(&mut server, &path, source);
        assert!(completions(&server, &uri, offset).is_empty(), "{source}");
    }
}

#[test]
fn local_receivers_shadow_standard_library_module_names() {
    let project = Project::new("shadow");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "use strings\nclass Item { var int count; } fun main() { var strings = Item(); strings.$CURSOR }",
    );
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, ["count"]);
}

#[test]
fn cached_rust_adaptors_provide_real_names_signatures_and_docs() {
    let project = Project::new("rust");
    project.file(".ntsc/ureq/ureq.export.json", r#"{
        "schema_version":3,"module":"ureq","functions":[
            {"name":"get","status":"typed","symbol":"ntsc_adaptor_get",
             "params":[{"name":"url","type":"string"}],"return":{"type":"object[Request]"},"doc":"Create a request."},
            {"name":"unsupported","status":"unsupported","params":null,"return":null}
        ]}"#);
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "use @ureq\nfun main() { @ureq.$CURSOR }",
    );
    let items = completions(&server, &uri, offset);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].label, "get");
    assert!(items[0].detail.as_ref().unwrap().contains("string url"));
    assert!(items[0].documentation.is_some());
}

#[test]
fn workspace_entry_and_vendored_modules_are_indexed_when_a_leaf_opens() {
    let project = Project::new("entry");
    project.file(
        "main.nt",
        "use \"src/leaf.nt\"\nuse toml\nfun from_entry() -> int { return 1; }\n",
    );
    project.file(".ntsc/toml/lib.nt", "fun decode() -> int { return 2; }");
    let path = project.file("src/leaf.nt", "fun leaf() {}");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(&mut server, &path, "fun leaf() { from_$CURSOR }");
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"from_entry".to_string()), "{labels:?}");
    assert!(
        server
            .workspace_files
            .values()
            .any(|file| file.uri.path().ends_with("/toml/lib.nt"))
    );
}

#[test]
fn separate_projects_and_private_declarations_do_not_leak_completions() {
    let left = Project::new("left");
    let right = Project::new("right");
    left.file(
        "helper.nt",
        "private fun hidden() {} fun public_helper() {}",
    );
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let right_path = right.file("main.nt", "");
    open(&mut server, &right_path, "fun foreign_name() {}");
    let path = left.file("main.nt", "");
    let (uri, offset) = open(
        &mut server,
        &path,
        "use \"helper.nt\"\nfun main() { $CURSOR }",
    );
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"public_helper".to_string()), "{labels:?}");
    assert!(!labels.contains(&"hidden".to_string()), "{labels:?}");
    assert!(!labels.contains(&"foreign_name".to_string()), "{labels:?}");
}

#[test]
fn multiple_utf16_changes_are_applied_in_order_and_stale_versions_are_rejected() {
    let uri = Url::parse("file:///unicode.nt").unwrap();
    let mut document = Document::new(
        uri,
        1,
        "var string text = \"😀\";\nvar int count = 1;\n".to_string(),
    );
    let first_start = document
        .index
        .position(&document.text, document.text.find('😀').unwrap());
    let first_end = lsp_types::Position::new(first_start.line, first_start.character + 2);
    let second_line = document.text.lines().nth(1).unwrap();
    let number = second_line.find('1').unwrap() as u32;
    assert!(document.apply_changes(
        2,
        vec![
            lsp_types::TextDocumentContentChangeEvent {
                range: Some(Range::new(first_start, first_end)),
                range_length: None,
                text: "あ".to_string(),
            },
            lsp_types::TextDocumentContentChangeEvent {
                range: Some(Range::new(
                    lsp_types::Position::new(1, number),
                    lsp_types::Position::new(1, number + 1)
                )),
                range_length: None,
                text: "2".to_string(),
            },
        ]
    ));
    assert!(document.text.contains("\"あ\""));
    assert!(document.text.contains("count = 2"));
    assert!(!document.apply_changes(
        1,
        vec![lsp_types::TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "stale".to_string()
        },]
    ));
    assert!(!document.text.contains("stale"));
}

#[test]
fn member_definition_and_rename_do_not_touch_same_named_methods_on_other_classes() {
    let project = Project::new("rename");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let source = "class A { fun run(int first) {} } class B { fun run(string other) {} } fun main() { var a = A(); var b = B(); a.ru$CURSORn(1); b.run(\"x\"); }";
    let (uri, offset) = open(&mut server, &path, source);
    let document = server.document_for(&uri).unwrap();
    let (_, symbol) = server.resolved_symbol_at(document, offset).unwrap();
    assert_eq!(symbol.container.as_deref(), Some("A"));
    let locations = server.symbol_references(document, offset, true).unwrap();
    assert_eq!(locations.len(), 2, "{locations:?}");
}

#[test]
fn comments_and_strings_do_not_offer_code_completions() {
    let project = Project::new("comments");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    for source in [
        "fun hello() {}\n// hel$CURSOR",
        "fun hello() {} fun main() { say(\"hel$CURSORlo\"); }",
    ] {
        let (uri, offset) = open(&mut server, &path, source);
        assert!(completions(&server, &uri, offset).is_empty(), "{source}");
    }
}

#[test]
fn namespaces_offer_exports_and_preserve_factory_receiver_types() {
    let project = Project::new("namespace");
    project.file(".ntsc/model/lib.nt",
        "class Item { var int count; } private fun hidden() {} fun make() -> Item { return Item(); }");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "use model\nfun main() { model.$CURSOR }",
    );
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"make".to_string()), "{labels:?}");
    assert!(!labels.contains(&"hidden".to_string()), "{labels:?}");
    let (uri, offset) = open(
        &mut server,
        &path,
        "use model\nfun main() { model.make().$CURSOR }",
    );
    let labels: Vec<_> = completions(&server, &uri, offset)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, ["count"]);
    let (uri, offset) = open(&mut server, &path, "use model\nfun main() { ma$CURSOR }");
    assert!(
        !completions(&server, &uri, offset)
            .iter()
            .any(|item| item.label == "make")
    );
}

#[test]
fn inherited_members_and_super_references_use_the_right_class() {
    let project = Project::new("inheritance");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let source = "class Parent { fun run(int parent_arg) {} } class Child extends Parent { fun run(string child_arg) {} fun check() { super.ru$CURSORn(1); } }";
    let (uri, offset) = open(&mut server, &path, source);
    let document = server.document_for(&uri).unwrap();
    let (_, symbol) = server.resolved_symbol_at(document, offset).unwrap();
    assert_eq!(symbol.container.as_deref(), Some("Parent"));
}

#[test]
fn workspace_discovery_is_not_limited_to_sixty_four_imports() {
    let project = Project::new("large");
    let mut source = String::new();
    for index in 0..100 {
        source.push_str(&format!("use \"module{index}.nt\"\n"));
        project.file(
            &format!("module{index}.nt"),
            &format!("fun function{index}() {{}}\n"),
        );
    }
    source.push_str("fun main() { function0$CURSOR }");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(&mut server, &path, &source);
    assert_eq!(server.workspace_files.len(), 100);
    assert!(
        completions(&server, &uri, offset)
            .iter()
            .any(|item| item.label == "function0")
    );
}

#[test]
fn unchanged_imports_are_reused_and_unrelated_open_files_are_excluded() {
    let project = Project::new("reuse");
    project.file("helper.nt", "fun imported() {}");
    let path = project.file("main.nt", "use \"helper.nt\"\nfun main() {}");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    open(&mut server, &path, "use \"helper.nt\"\nfun main() {}");
    let previous = server
        .workspace_files
        .values()
        .find(|file| file.uri.path().ends_with("helper.nt"))
        .unwrap()
        .analysis
        .tokens
        .as_ptr();
    let unrelated = project.file("unrelated.nt", "");
    open(&mut server, &unrelated, "fun unrelated_name() {}");
    let (uri, offset) = open(
        &mut server,
        &path,
        "use \"helper.nt\"\nfun main() { unrelated_$CURSOR }",
    );
    let current = server
        .workspace_files
        .values()
        .find(|file| file.uri.path().ends_with("helper.nt"))
        .unwrap()
        .analysis
        .tokens
        .as_ptr();
    assert_eq!(previous, current);
    assert!(
        !completions(&server, &uri, offset)
            .iter()
            .any(|item| item.label == "unrelated_name")
    );
}

#[test]
fn malformed_requests_receive_an_error_response_instead_of_hanging() {
    let (connection, client) = Connection::memory();
    let mut server = Server::new(&connection);
    server.handle_request(Request::new(
        RequestId::from(5),
        "textDocument/completion".to_string(),
        serde_json::json!({"invalid":true}),
    ));
    let Message::Response(response) = client.receiver.recv().unwrap() else {
        panic!("expected response");
    };
    assert_eq!(response.id, RequestId::from(5));
    assert_eq!(
        response.error.unwrap().code,
        lsp_server::ErrorCode::InvalidParams as i32
    );
}

#[test]
fn workspace_folders_are_indexed_before_documents_open() {
    let project = Project::new("folders");
    project.file("main.nt", "fun available() {}");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    server.workspace_roots.push(project.directory.clone());
    server.refresh_workspace_files();
    assert!(server.workspace_files.values().any(|file| {
        file.analysis
            .symbols
            .iter()
            .any(|symbol| symbol.name == "available")
    }));
}

#[test]
fn signature_help_stops_after_the_call_closes() {
    let project = Project::new("signature");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "fun add(int a, int b) -> int { return a + b; } fun main() { add(1, 2); $CURSOR }",
    );
    let document = server.document_for(&uri).unwrap();
    assert!(document.analysis.enclosing_call(offset).is_none());
}

#[test]
fn signature_help_tracks_arguments_in_normal_and_awaited_calls() {
    let project = Project::new("call-signature");
    project.file(
        ".ntsc/ureq/ureq.export.json",
        r#"{"module":"ureq","functions":[
        {"name":"get_async","status":"typed","symbol":"ntsc_get_async","async":true,
         "params":[{"name":"url","type":"string"},{"name":"timeout","type":"int"}],
         "return":{"type":"object[Response]"},"doc":"Fetch asynchronously."}
    ]}"#,
    );
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    for (source, expected_parameter) in [
        (
            "fun add(int first, int second) -> int { return first + second; } fun main() { add(1, $CURSOR); }",
            1,
        ),
        (
            "use @ureq\nasync fun main() { await @ureq.get_async(\"url\", $CURSOR); }",
            1,
        ),
        (
            "fun add(int first, int second) -> int { return first + second; } fun main() { add(  $CURSOR); }",
            0,
        ),
        (
            "fun add(int first, int second) -> int { return first + second; } fun main() { add($CURSOR); var add = 1; }",
            0,
        ),
        (
            "fun add(int first, int second) -> int { return first + second; } fun main() { { var add = 1; } add($CURSOR); }",
            0,
        ),
        (
            "async fun add(int first, int second) -> int { return first + second; } async fun main() { await add($CURSOR); }",
            0,
        ),
    ] {
        let (uri, offset) = open(&mut server, &path, source);
        let document = server.document_for(&uri).unwrap();
        let help = server
            .signature_help(lsp_types::SignatureHelpParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: document.index.position(&document.text, offset),
                },
                work_done_progress_params: Default::default(),
                context: None,
            })
            .unwrap();
        assert_eq!(
            help.active_parameter,
            Some(expected_parameter),
            "{source}: {help:?}"
        );
        assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 2);
    }
}

#[test]
fn signature_help_does_not_guess_private_namespaced_or_shadowed_functions() {
    let project = Project::new("signature-binding");
    project.file(
        "library.nt",
        "private fun hidden(int value) {} fun exposed(int value) {}",
    );
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    for source in [
        "use \"library.nt\"\nfun main() { hidden($CURSOR); }",
        "use \"library.nt\" as library\nfun main() { exposed($CURSOR); }",
        "fun exposed(int value) {} fun main() { var exposed = 1; exposed($CURSOR); }",
        "fun exposed(int value) {} fun main(int exposed) { exposed($CURSOR); }",
        "use \"library.nt\"\nfun main() { var exposed = 1; exposed(  $CURSOR); }",
        "fun exposed(int value) {} fun main() { { var exposed = 1; exposed($CURSOR); } }",
        "async fun exposed(int value) {} async fun main() { var exposed = 1; await exposed($CURSOR); }",
        "fun exposed(int value) {} fun main() { var exposed = 1; exposed($CURSOR",
    ] {
        let (uri, offset) = open(&mut server, &path, source);
        let document = server.document_for(&uri).unwrap();
        let help = server.signature_help(lsp_types::SignatureHelpParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                position: document.index.position(&document.text, offset),
            },
            work_done_progress_params: Default::default(),
            context: None,
        });
        assert!(help.is_none(), "{source}: {help:?}");
    }
}

#[test]
fn definitions_resolve_factory_call_receivers_without_truncated_source_spans() {
    let project = Project::new("factory-definition");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "class Item { fun ping() {} } fun make() -> Item { return Item(); } fun main() { make().pi$CURSORng(); }",
    );
    let document = server.document_for(&uri).unwrap();
    let (_, symbol) = server.resolved_symbol_at(document, offset).unwrap();
    assert_eq!(symbol.container.as_deref(), Some("Item"));
}

#[test]
fn clients_without_snippet_support_do_not_receive_snippet_placeholders() {
    let project = Project::new("snippets");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(&mut server, &path, "fu$CURSOR");
    assert!(
        completions(&server, &uri, offset)
            .iter()
            .all(|item| item.insert_text_format != Some(InsertTextFormat::SNIPPET))
    );
}

#[test]
fn rename_rejects_keywords_and_colliding_declarations() {
    let project = Project::new("rename-safety");
    let path = project.file("main.nt", "");
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    let (uri, offset) = open(
        &mut server,
        &path,
        "fun existing() {} fun ta$CURSORrget() {}",
    );
    let position = server
        .document_for(&uri)
        .unwrap()
        .index
        .position(&server.document_for(&uri).unwrap().text, offset);
    for name in ["while", "existing"] {
        let response = server.rename(
            RequestId::from(8),
            lsp_types::RenameParams {
                text_document_position: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                new_name: name.to_string(),
                work_done_progress_params: Default::default(),
            },
        );
        assert!(response.error.is_some(), "{name}");
    }
}

#[test]
#[ignore = "requires NTSC_LSP_PROJECT pointing to a local Neutron project"]
fn inspect_real_project() {
    let root = PathBuf::from(std::env::var_os("NTSC_LSP_PROJECT").expect("NTSC_LSP_PROJECT"));
    let config =
        ntsc_build::parse(&fs::read_to_string(root.join("neutron.toml")).unwrap()).unwrap();
    let path = root.join(config.entry);
    let text = fs::read_to_string(&path).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let (connection, _) = Connection::memory();
    let mut server = Server::new(&connection);
    server
        .documents
        .insert(uri.to_string(), Document::new(uri.clone(), 1, text));
    server.refresh_workspace_files();
    let modules: Vec<_> = server.metadata.modules(&root).collect();
    assert!(!modules.is_empty(), "expected built-project Rust metadata");
    let functions = modules
        .iter()
        .flat_map(|module| &module.functions)
        .filter(|function| function.callable())
        .count();
    assert!(functions > 0);
    println!(
        "{} imported Neutron files, {} cached Rust modules, {} callable Rust signatures",
        server.workspace_files.len(),
        modules.len(),
        functions
    );
    let document = server.document_for(&uri).unwrap();
    assert!(
        document.analysis.parse_errors.is_empty(),
        "{:?}",
        document.analysis.parse_errors
    );
    let candidate = server.all_views().into_iter().find_map(|view| {
        view.analysis
            .references
            .iter()
            .find(|reference| {
                reference.receiver_source.as_ref().is_some_and(
                    |source| matches!(source, ValueSource::Name(name, _) if name.starts_with('@')),
                ) && modules.iter().any(|module| {
                    module
                        .functions
                        .iter()
                        .any(|function| function.name == reference.name && function.callable())
                })
            })
            .map(|reference| {
                (
                    view.uri.clone(),
                    view.text.to_string(),
                    reference.span.start,
                    reference.name.clone(),
                )
            })
    });
    if let Some((file_uri, source, offset, name)) = candidate {
        server.documents.insert(
            file_uri.to_string(),
            Document::new(file_uri.clone(), 1, source),
        );
        server.refresh_workspace_files();
        let items = completions(&server, &file_uri, offset);
        assert!(
            items.iter().any(|item| item.label == name),
            "missing real Rust API completion: {name}"
        );
        println!("Verified completion for {name}");
    } else {
        panic!("no callable Rust API reference found in the built project");
    }
}
