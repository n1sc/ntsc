//! LSP server loop over stdio, backed by [`crate::analysis`].
//!
//! One [`FileAnalysis`] is kept per open document; every `didOpen` /
//! `didChange` re-analyzes the buffer and republishes diagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentLinkRequest,
    DocumentSymbolRequest, ExecuteCommand, FoldingRangeRequest, Formatting, GotoDefinition,
    HoverRequest, InlayHintRequest, PrepareRenameRequest, References, Rename,
    SelectionRangeRequest, SemanticTokensFullRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeActionKind,
    CodeActionOptions, CodeActionProviderCapability, CompletionItem, CompletionItemKind,
    CompletionOptions, CompletionResponse, DiagnosticSeverity, DocumentFormattingOptions,
    DocumentHighlight, DocumentHighlightKind, DocumentLink, DocumentLinkOptions, DocumentSymbol,
    ExecuteCommandOptions, FoldingRange, FoldingRangeKind, FoldingRangeProviderCapability, Hover,
    HoverContents, HoverProviderCapability, InlayHint, InlayHintKind, InlayHintLabel,
    InlayHintOptions, InsertTextFormat, Location, MarkupContent, MarkupKind, NumberOrString, OneOf,
    ParameterInformation, PositionEncodingKind, PrepareRenameResponse, PublishDiagnosticsParams,
    Range, RenameOptions, SelectionRange, SelectionRangeProviderCapability, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensResult,
    ServerCapabilities, SignatureHelp, SignatureHelpOptions, SignatureInformation,
    SymbolInformation, SymbolKind as LspSymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url, WorkspaceEdit,
};

use crate::analysis::{FileAnalysis, Symbol, SymbolKind, analyze};
use crate::convert::LineIndex;
use crate::metadata::{MetadataCache, project_root};
use crate::semantic::{SemanticContext, SemanticIndex, SemanticView, ValueSource};
use crate::stdlib;

const SERVER_NAME: &str = "ntsc-lsp";
const DIAGNOSTIC_SOURCE: &str = "ntsc";

/// Workspace command listing every `test` block across open documents and
/// imported files, so editors can discover and run `ntsc test` targets.
pub const LIST_TESTS_COMMAND: &str = "neutron.listTests";

/// Run the language server on stdio; returns the process exit code.
pub fn run_stdio() -> i32 {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = match serde_json::to_value(server_capabilities()) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            eprintln!("{SERVER_NAME}: failed to encode capabilities: {error}");
            return 1;
        }
    };
    let init_params = match connection.initialize(capabilities) {
        Ok(params) => params,
        Err(error) => {
            eprintln!("{SERVER_NAME}: initialize failed: {error}");
            return 1;
        }
    };
    let mut server = Server::new(&connection);
    match serde_json::from_value::<lsp_types::InitializeParams>(init_params) {
        Ok(params) => {
            server.snippets_supported = params
                .capabilities
                .text_document
                .and_then(|document| document.completion)
                .and_then(|completion| completion.completion_item)
                .and_then(|item| item.snippet_support)
                .unwrap_or(false);
            if let Some(folders) = params.workspace_folders {
                server.workspace_roots.extend(
                    folders
                        .into_iter()
                        .filter_map(|folder| folder.uri.to_file_path().ok()),
                );
            } else {
                // Older LSP clients provide rootUri instead of workspace folders.
                #[allow(deprecated)]
                if let Some(uri) = params.root_uri
                    && let Ok(path) = uri.to_file_path()
                {
                    server.workspace_roots.push(path);
                }
            }
            server.refresh_workspace_files();
            if params
                .capabilities
                .workspace
                .and_then(|workspace| workspace.did_change_watched_files)
                .and_then(|files| files.dynamic_registration)
                .unwrap_or(false)
            {
                let registration = serde_json::json!({"registrations":[{
                    "id":"ntsc-watch-files","method":"workspace/didChangeWatchedFiles",
                    "registerOptions":{"watchers":[
                        {"globPattern":"**/*.nt"},{"globPattern":"**/*.export.json"},
                        {"globPattern":"**/neutron.toml"}
                    ]}
                }]});
                if let Err(error) = server.sender.send(Message::Request(Request::new(
                    RequestId::from("ntsc-watch-files".to_string()),
                    "client/registerCapability".to_string(),
                    registration,
                ))) {
                    eprintln!("{SERVER_NAME}: cannot register file watcher: {error}");
                }
            }
        }
        Err(error) => eprintln!("{SERVER_NAME}: invalid initialization parameters: {error}"),
    }
    server.main_loop(&connection);
    // Drop the server (and its cloned sender) plus the connection so the
    // IO threads observe a closed channel and terminate; otherwise `join`
    // blocks forever.
    drop(server);
    drop(connection);
    if io_threads.join().is_err() {
        eprintln!("{SERVER_NAME}: IO threads failed");
        return 1;
    }
    0
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            lsp_types::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(lsp_types::TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        workspace: Some(lsp_types::WorkspaceServerCapabilities {
            workspace_folders: Some(lsp_types::WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            ..Default::default()
        }),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
            ..CompletionOptions::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        document_formatting_provider: Some(OneOf::Right(DocumentFormattingOptions {
            work_done_progress_options: Default::default(),
        })),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(
            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                SemanticTokensOptions {
                    legend: token_legend(),
                    range: None,
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    work_done_progress_options: Default::default(),
                },
            ),
        ),
        inlay_hint_provider: Some(OneOf::Right(
            lsp_types::InlayHintServerCapabilities::Options(InlayHintOptions {
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            }),
        )),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            work_done_progress_options: Default::default(),
            resolve_provider: None,
        })),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![LIST_TESTS_COMMAND.to_string()],
            work_done_progress_options: Default::default(),
        }),
        ..ServerCapabilities::default()
    }
}

// ── Documents ─────────────────────────────────────────────────────────────

struct Document {
    uri: Url,
    version: i32,
    text: String,
    analysis: FileAnalysis,
    semantic: SemanticIndex,
    index: LineIndex,
}

impl Document {
    fn new(uri: Url, version: i32, text: String) -> Self {
        let analysis = analyze(&text);
        let semantic = SemanticIndex::new(&analysis);
        let index = LineIndex::new(&text);
        Self {
            uri,
            version,
            text,
            analysis,
            semantic,
            index,
        }
    }

    fn update(&mut self, version: i32, text: String) {
        self.version = version;
        self.analysis = analyze(&text);
        self.semantic = SemanticIndex::new(&self.analysis);
        self.index = LineIndex::new(&text);
        self.text = text;
    }

    fn offset(&self, position: lsp_types::Position) -> Option<usize> {
        self.index.offset(&self.text, position)
    }

    fn apply_changes(
        &mut self,
        version: i32,
        changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
    ) -> bool {
        if version <= self.version {
            return false;
        }
        let mut text = self.text.clone();
        for change in changes {
            if let Some(range) = change.range {
                let index = LineIndex::new(&text);
                let (Some(start), Some(end)) = (
                    index.offset(&text, range.start),
                    index.offset(&text, range.end),
                ) else {
                    eprintln!("{SERVER_NAME}: invalid document change range");
                    return false;
                };
                if start > end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                    eprintln!("{SERVER_NAME}: invalid document change boundaries");
                    return false;
                }
                text.replace_range(start..end, &change.text);
            } else {
                text = change.text;
            }
        }
        self.update(version, text);
        true
    }

    fn range(&self, start: usize, end: usize) -> Range {
        self.index.range(&self.text, start, end)
    }
}

struct Server<'a> {
    sender: crossbeam_channel::Sender<Message>,
    documents: HashMap<String, Document>,
    /// Files pulled in via `use "other.nt"` that are not open in the
    /// client, loaded from disk so definition, references, rename, and
    /// symbols work across module boundaries.
    workspace_files: HashMap<String, WorkspaceFile>,
    reachable_files: HashMap<String, std::collections::HashSet<String>>,
    metadata: MetadataCache,
    workspace_roots: Vec<PathBuf>,
    snippets_supported: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

/// A non-open file loaded from disk through the import graph.
struct WorkspaceFile {
    uri: Url,
    text: String,
    analysis: FileAnalysis,
    semantic: SemanticIndex,
    modified: Option<std::time::SystemTime>,
    index: LineIndex,
}

/// A uniform view over open documents and disk-loaded workspace files.
#[derive(Clone, Copy)]
struct DocView<'x> {
    uri: &'x Url,
    text: &'x str,
    analysis: &'x FileAnalysis,
    semantic: &'x SemanticIndex,
    index: &'x LineIndex,
}

impl<'a> Server<'a> {
    fn new(connection: &'a Connection) -> Self {
        Self {
            sender: connection.sender.clone(),
            documents: HashMap::new(),
            workspace_files: HashMap::new(),
            reachable_files: HashMap::new(),
            metadata: MetadataCache::default(),
            workspace_roots: Vec::new(),
            snippets_supported: false,
            _marker: std::marker::PhantomData,
        }
    }

    fn all_views(&self) -> Vec<DocView<'_>> {
        let mut views = Vec::new();
        for document in self.documents.values() {
            views.push(DocView {
                uri: &document.uri,
                text: &document.text,
                analysis: &document.analysis,
                semantic: &document.semantic,
                index: &document.index,
            });
        }
        for file in self.workspace_files.values() {
            views.push(DocView {
                uri: &file.uri,
                text: &file.text,
                analysis: &file.analysis,
                semantic: &file.semantic,
                index: &file.index,
            });
        }
        views
    }

    fn relevant_views(&self, document: &Document) -> Vec<DocView<'_>> {
        let Ok(path) = document.uri.to_file_path() else {
            return self
                .all_views()
                .into_iter()
                .filter(|view| view.uri == &document.uri)
                .collect();
        };
        let root = project_root(&path);
        self.all_views()
            .into_iter()
            .filter(|view| {
                view.uri == &document.uri
                    || (self
                        .reachable_files
                        .get(document.uri.as_str())
                        .is_some_and(|files| files.contains(view.uri.as_str()))
                        && view
                            .uri
                            .to_file_path()
                            .is_ok_and(|path| path.starts_with(&root)))
            })
            .collect()
    }

    fn native_namespace<'b>(&'b self, view: DocView<'b>, name: &str) -> Option<DocView<'b>> {
        let file = view.uri.to_file_path().ok()?;
        let directory = file.parent()?;
        for statement in &view.analysis.program.as_ref()?.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                alias,
                is_file_path,
                is_rust: false,
                imported_symbols,
                ..
            } = statement
            {
                if !imported_symbols.is_empty() || (*is_file_path && alias.is_none()) {
                    continue;
                }
                if alias.as_ref().unwrap_or(library).lexeme() != name {
                    continue;
                }
                let mut path = if *is_file_path {
                    directory.join(library.lexeme())
                } else {
                    ntsc_build::modules::vendored_import(&project_root(&file), library.lexeme())?
                };
                if path.extension().is_none() {
                    path.set_extension("nt");
                }
                let path = normalize_path(&path);
                return self.all_views().into_iter().find(|candidate| {
                    candidate
                        .uri
                        .to_file_path()
                        .is_ok_and(|candidate| normalize_path(&candidate) == path)
                });
            }
        }
        None
    }

    fn semantic_context<'b>(&self, views: &[DocView<'b>], current: usize) -> SemanticContext<'b> {
        let mut namespaces = HashMap::new();
        let mut namespaced_files = std::collections::HashSet::new();
        for (index, view) in views.iter().enumerate() {
            for symbol in &view.analysis.symbols {
                if symbol.kind != SymbolKind::Module {
                    continue;
                }
                if let Some(namespace) = self.native_namespace(*view, &symbol.name)
                    && let Some(target) = views.iter().position(|view| view.uri == namespace.uri)
                {
                    namespaces.insert((index, symbol.name.clone()), target);
                    namespaced_files.insert(target);
                }
            }
        }
        SemanticContext {
            current,
            namespaces,
            namespaced_files,
            views: views
                .iter()
                .map(|view| SemanticView {
                    analysis: view.analysis,
                    index: view.semantic,
                })
                .collect(),
        }
    }

    fn main_loop(&mut self, connection: &Connection) {
        for message in &connection.receiver {
            match message {
                Message::Request(request) => {
                    if self.handle_request(request) {
                        break;
                    }
                }
                Message::Response(response) => {
                    if let Some(error) = response.error {
                        eprintln!("{SERVER_NAME}: client request failed: {}", error.message);
                    }
                }
                Message::Notification(notification) => {
                    if notification.method == "exit" {
                        break;
                    }
                    self.handle_notification(notification);
                }
            }
        }
    }

    /// Handle a request; returns `true` when the server should shut down.
    fn handle_request(&mut self, request: Request) -> bool {
        let method = request.method.clone();
        if method == "shutdown" {
            let _ = self
                .sender
                .send(Message::Response(Response::new_ok(request.id, ())));
            return true;
        }
        let mut pending: Option<Request> = Some(request);
        macro_rules! dispatch {
            ($request:ty, $params:ty, $handler:expr) => {
                if let Some(current) = pending.take() {
                    let invalid_id = current.id.clone();
                    match current
                        .extract::<$params>(<$request as lsp_types::request::Request>::METHOD)
                    {
                        Ok((request_id, params)) => {
                            let result = $handler(params);
                            let _ = self
                                .sender
                                .send(Message::Response(Response::new_ok(request_id, result)));
                            return false;
                        }
                        Err(lsp_server::ExtractError::MethodMismatch(mismatch)) => {
                            pending = Some(mismatch);
                        }
                        Err(lsp_server::ExtractError::JsonError { method, error }) => {
                            eprintln!("{SERVER_NAME}: invalid params for {method}: {error}");
                            if let Err(error) =
                                self.sender.send(Message::Response(Response::new_err(
                                    invalid_id,
                                    lsp_server::ErrorCode::InvalidParams as i32,
                                    error.to_string(),
                                )))
                            {
                                eprintln!(
                                    "{SERVER_NAME}: cannot send invalid-params response: {error}"
                                );
                            }
                            return false;
                        }
                    }
                }
            };
        }
        dispatch!(HoverRequest, lsp_types::HoverParams, |params| self
            .hover(params));
        dispatch!(Completion, lsp_types::CompletionParams, |params| self
            .completion(params));
        dispatch!(GotoDefinition, lsp_types::GotoDefinitionParams, |params| {
            self.goto_definition(params)
        });
        dispatch!(References, lsp_types::ReferenceParams, |params| {
            self.find_references(params)
        });
        dispatch!(
            DocumentHighlightRequest,
            lsp_types::DocumentHighlightParams,
            |params| self.document_highlight(params)
        );
        dispatch!(
            DocumentSymbolRequest,
            lsp_types::DocumentSymbolParams,
            |params| self.document_symbols(params)
        );
        dispatch!(
            WorkspaceSymbolRequest,
            lsp_types::WorkspaceSymbolParams,
            |params| self.workspace_symbols(params)
        );
        dispatch!(
            SignatureHelpRequest,
            lsp_types::SignatureHelpParams,
            |params| self.signature_help(params)
        );
        dispatch!(Formatting, lsp_types::DocumentFormattingParams, |params| {
            self.formatting(params)
        });
        dispatch!(
            FoldingRangeRequest,
            lsp_types::FoldingRangeParams,
            |params| self.folding_ranges(params)
        );
        dispatch!(
            SemanticTokensFullRequest,
            lsp_types::SemanticTokensParams,
            |params| self.semantic_tokens(params)
        );
        dispatch!(InlayHintRequest, lsp_types::InlayHintParams, |params| {
            self.inlay_hints(params)
        });
        dispatch!(CodeActionRequest, lsp_types::CodeActionParams, |params| {
            self.code_actions(params)
        });
        dispatch!(
            SelectionRangeRequest,
            lsp_types::SelectionRangeParams,
            |params| self.selection_ranges(params)
        );
        dispatch!(
            DocumentLinkRequest,
            lsp_types::DocumentLinkParams,
            |params| { self.document_links(params) }
        );
        dispatch!(
            CallHierarchyPrepare,
            lsp_types::CallHierarchyPrepareParams,
            |params| self.prepare_call_hierarchy(params)
        );
        dispatch!(
            CallHierarchyIncomingCalls,
            lsp_types::CallHierarchyIncomingCallsParams,
            |params| self.incoming_calls(params)
        );
        dispatch!(
            CallHierarchyOutgoingCalls,
            lsp_types::CallHierarchyOutgoingCallsParams,
            |params| self.outgoing_calls(params)
        );
        if let Some(current) = pending.take() {
            match current.extract::<lsp_types::ExecuteCommandParams>(
                <ExecuteCommand as lsp_types::request::Request>::METHOD,
            ) {
                Ok((request_id, params)) => {
                    let response = self.execute_command(request_id, params);
                    let _ = self.sender.send(Message::Response(response));
                    return false;
                }
                Err(lsp_server::ExtractError::MethodMismatch(mismatch)) => {
                    pending = Some(mismatch);
                }
                Err(lsp_server::ExtractError::JsonError { method, error }) => {
                    eprintln!("{SERVER_NAME}: invalid params for {method}: {error}");
                    return false;
                }
            }
        }
        if let Some(current) = pending.take() {
            match current
                .extract::<lsp_types::RenameParams>(<Rename as lsp_types::request::Request>::METHOD)
            {
                Ok((request_id, params)) => {
                    let response = self.rename(request_id, params);
                    let _ = self.sender.send(Message::Response(response));
                    return false;
                }
                Err(lsp_server::ExtractError::MethodMismatch(mismatch)) => {
                    pending = Some(mismatch);
                }
                Err(lsp_server::ExtractError::JsonError { method, error }) => {
                    eprintln!("{SERVER_NAME}: invalid params for {method}: {error}");
                    return false;
                }
            }
        }
        if let Some(current) = pending.take() {
            match current.extract::<lsp_types::TextDocumentPositionParams>(
                <PrepareRenameRequest as lsp_types::request::Request>::METHOD,
            ) {
                Ok((request_id, params)) => {
                    let result = self.prepare_rename(params);
                    let _ = self
                        .sender
                        .send(Message::Response(Response::new_ok(request_id, result)));
                    return false;
                }
                Err(lsp_server::ExtractError::MethodMismatch(mismatch)) => {
                    pending = Some(mismatch);
                }
                Err(lsp_server::ExtractError::JsonError { method, error }) => {
                    eprintln!("{SERVER_NAME}: invalid params for {method}: {error}");
                    return false;
                }
            }
        }
        if let Some(unhandled) = pending {
            let _ = self.sender.send(Message::Response(Response {
                id: unhandled.id,
                result: None,
                error: Some(lsp_server::ResponseError {
                    code: lsp_server::ErrorCode::MethodNotFound as i32,
                    message: format!("unsupported method: {method}"),
                    data: None,
                }),
            }));
        }
        false
    }

    fn handle_notification(&mut self, notification: Notification) {
        if let Ok(params) = serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(
            notification.params.clone(),
        ) && notification.method == "textDocument/didOpen"
        {
            let uri = params.text_document.uri;
            let document = Document::new(
                uri.clone(),
                params.text_document.version,
                params.text_document.text,
            );
            self.documents.insert(uri.as_str().to_string(), document);
            // Load the import closure before diagnosing: cross-module
            // names must not read as undefined on open.
            self.refresh_workspace_files();
            if let Some(document) = self.documents.get(uri.as_str()) {
                self.publish_diagnostics(document);
            }
        } else if notification.method == "textDocument/didChange" {
            if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                notification.params.clone(),
            ) {
                let key = params.text_document.uri.as_str().to_string();
                if let Some(document) = self.documents.get_mut(&key) {
                    if !document.apply_changes(params.text_document.version, params.content_changes)
                    {
                        return;
                    }
                    self.refresh_workspace_files();
                    for document in self.documents.values() {
                        self.publish_diagnostics(document);
                    }
                }
            }
        } else if notification.method == "textDocument/didClose"
            && let Ok(params) = serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(
                notification.params.clone(),
            )
        {
            let key = params.text_document.uri.as_str().to_string();
            let _ = self.documents.remove(&key);
            self.refresh_workspace_files();
            let clear = PublishDiagnosticsParams {
                uri: params.text_document.uri,
                diagnostics: Vec::new(),
                version: None,
            };
            match serde_json::to_value(clear) {
                Ok(params) => {
                    if let Err(error) = self.sender.send(Message::Notification(Notification::new(
                        "textDocument/publishDiagnostics".to_string(),
                        params,
                    ))) {
                        eprintln!("{SERVER_NAME}: cannot clear diagnostics: {error}");
                    }
                }
                Err(error) => eprintln!("{SERVER_NAME}: cannot encode diagnostics: {error}"),
            }
        } else if notification.method == "workspace/didChangeWorkspaceFolders" {
            if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeWorkspaceFoldersParams>(
                notification.params,
            ) {
                for folder in params.event.removed {
                    if let Ok(path) = folder.uri.to_file_path() {
                        self.workspace_roots.retain(|root| root != &path);
                    }
                }
                for folder in params.event.added {
                    if let Ok(path) = folder.uri.to_file_path()
                        && !self.workspace_roots.contains(&path)
                    {
                        self.workspace_roots.push(path);
                    }
                }
                self.refresh_workspace_files();
            }
        } else if matches!(
            notification.method.as_str(),
            "textDocument/didSave" | "workspace/didChangeWatchedFiles"
        ) {
            self.refresh_workspace_files();
            for document in self.documents.values() {
                self.publish_diagnostics(document);
            }
        }
    }

    /// Rebuild the disk-backed import closure of every open document.
    ///
    /// Files reached through `use "other.nt"` are analyzed from disk (open
    /// buffers always win) so cross-module definition, references, rename,
    /// and symbols keep working while typing.
    fn refresh_workspace_files(&mut self) {
        use std::collections::HashSet;
        let mut stack: Vec<(PathBuf, String)> = Vec::new();
        let mut roots = Vec::new();
        let mut entries = HashMap::new();
        for document in self.documents.values() {
            if let Ok(path) = document.uri.to_file_path()
                && let Some(dir) = path.parent()
            {
                let root = project_root(&path);
                if !roots.contains(&root) {
                    match std::fs::read_to_string(root.join("neutron.toml")) {
                        Ok(source) => match ntsc_build::parse(&source) {
                            Ok(config) => {
                                let entry = root.join(config.entry);
                                if entry.is_file() {
                                    if let Ok(uri) = Url::from_file_path(&entry) {
                                        entries.insert(root.clone(), uri.to_string());
                                    }
                                    stack
                                        .push((root.clone(), entry.to_string_lossy().into_owned()));
                                }
                            }
                            Err(errors) => {
                                eprintln!("{SERVER_NAME}: invalid project manifest: {errors:?}")
                            }
                        },
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            eprintln!("{SERVER_NAME}: cannot read project manifest: {error}")
                        }
                    }
                    roots.push(root);
                }
                for import in file_imports_of(&document.analysis, dir) {
                    stack.push((dir.to_path_buf(), import));
                }
            }
        }
        for root in &self.workspace_roots {
            if roots.contains(root) {
                continue;
            }
            match std::fs::read_to_string(root.join("neutron.toml")) {
                Ok(source) => match ntsc_build::parse(&source) {
                    Ok(config) => {
                        let entry = root.join(config.entry);
                        if entry.is_file() {
                            if let Ok(uri) = Url::from_file_path(&entry) {
                                entries.insert(root.clone(), uri.to_string());
                            }
                            stack.push((root.clone(), entry.to_string_lossy().into_owned()));
                        }
                    }
                    Err(errors) => eprintln!("{SERVER_NAME}: invalid project manifest: {errors:?}"),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => eprintln!("{SERVER_NAME}: cannot read project manifest: {error}"),
            }
            roots.push(root.clone());
        }
        self.metadata.refresh(&roots);
        let mut previous = std::mem::take(&mut self.workspace_files);
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut loaded: HashMap<String, WorkspaceFile> = HashMap::new();
        while let Some((dir, import)) = stack.pop() {
            if loaded.len() >= MAX_WORKSPACE_FILES {
                break;
            }
            let mut candidate = dir.join(&import);
            if candidate.extension().is_none() {
                candidate.set_extension("nt");
            }
            let normalized = normalize_path(&candidate);
            if !seen.insert(normalized.clone()) {
                continue;
            }
            let Ok(uri) = Url::from_file_path(&normalized) else {
                continue;
            };
            if self.documents.contains_key(uri.as_str()) {
                continue;
            }
            let metadata = match std::fs::metadata(&normalized) {
                Ok(metadata) if metadata.len() <= MAX_WORKSPACE_FILE_BYTES as u64 => metadata,
                Ok(_) => continue,
                Err(error) => {
                    eprintln!(
                        "{SERVER_NAME}: cannot read {}: {error}",
                        normalized.display()
                    );
                    continue;
                }
            };
            let modified = match metadata.modified() {
                Ok(modified) => Some(modified),
                Err(error) => {
                    eprintln!(
                        "{SERVER_NAME}: cannot inspect {}: {error}",
                        normalized.display()
                    );
                    None
                }
            };
            if let Some(cached) = previous.remove(uri.as_str())
                && modified.is_some()
                && cached.modified == modified
                && cached.text.len() as u64 == metadata.len()
            {
                if let Some(parent) = normalized.parent() {
                    for nested in file_imports_of(&cached.analysis, parent) {
                        stack.push((parent.to_path_buf(), nested));
                    }
                }
                loaded.insert(uri.as_str().to_string(), cached);
                continue;
            }
            let text = match std::fs::read_to_string(&normalized) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!(
                        "{SERVER_NAME}: cannot read {}: {error}",
                        normalized.display()
                    );
                    continue;
                }
            };
            let analysis = analyze(&text);
            let semantic = SemanticIndex::new(&analysis);
            if let Some(parent) = normalized.parent() {
                for nested in file_imports_of(&analysis, parent) {
                    stack.push((parent.to_path_buf(), nested));
                }
            }
            let index = LineIndex::new(&text);
            loaded.insert(
                uri.as_str().to_string(),
                WorkspaceFile {
                    uri,
                    text,
                    analysis,
                    semantic,
                    modified,
                    index,
                },
            );
        }
        self.workspace_files = loaded;
        let mut edges = HashMap::new();
        for view in self.all_views() {
            let Ok(path) = view.uri.to_file_path() else {
                continue;
            };
            let Some(directory) = path.parent() else {
                continue;
            };
            let imports = file_imports_of(view.analysis, directory)
                .into_iter()
                .filter_map(|import| {
                    let mut path = directory.join(import);
                    if path.extension().is_none() {
                        path.set_extension("nt");
                    }
                    Url::from_file_path(normalize_path(&path))
                        .ok()
                        .map(|uri| uri.to_string())
                })
                .collect::<Vec<_>>();
            edges.insert(view.uri.to_string(), imports);
        }
        self.reachable_files.clear();
        for document in self.documents.values() {
            let mut stack = vec![document.uri.to_string()];
            if let Ok(path) = document.uri.to_file_path()
                && let Some(entry) = entries.get(&project_root(&path))
            {
                stack.push(entry.clone());
            }
            let mut reached = HashSet::new();
            while let Some(uri) = stack.pop() {
                if !reached.insert(uri.clone()) {
                    continue;
                }
                if let Some(imports) = edges.get(&uri) {
                    stack.extend(imports.iter().cloned());
                }
            }
            self.reachable_files
                .insert(document.uri.to_string(), reached);
        }
    }

    fn document_for(&self, uri: &Url) -> Option<&Document> {
        self.documents.get(uri.as_str())
    }

    /// Whether any analyzed file defines `name` as a namespace-visible
    /// symbol (i.e. anything but a local variable or parameter).
    fn workspace_knows(&self, document: &Document, name: &str) -> bool {
        self.relevant_views(document).iter().any(|view| {
            view.analysis.symbols.iter().any(|symbol| {
                symbol.name == name
                    && is_cross_file(symbol)
                    && !view
                        .semantic
                        .private_symbols
                        .contains(&symbol.name_span.start)
            })
        })
    }

    // ── Diagnostics ──

    fn publish_diagnostics(&self, document: &Document) {
        let mut diagnostics = Vec::new();
        let analysis = &document.analysis;
        for error in &analysis.parse_errors {
            diagnostics.push(lsp_diagnostic(
                document.range(error.span.start, error.span.end),
                DiagnosticSeverity::ERROR,
                Some(ntsc_diag::codes::PARSE),
                &error.message,
                None,
            ));
        }
        // Each file is analyzed alone, so a name imported from another
        // module reads as undefined. When the import closure actually
        // defines it, the error is an artifact of single-file analysis —
        // suppress it rather than crying wolf on every cross-module call.
        for error in &analysis.resolve_errors {
            if let Some(name) = undefined_name_of(&error.message)
                && self.workspace_knows(document, name)
            {
                continue;
            }
            let message = match &error.suggestion {
                Some(suggestion) => format!("{}\ndid you mean `{suggestion}`?", error.message),
                None => error.message.clone(),
            };
            diagnostics.push(lsp_diagnostic(
                document.range(error.span.start, error.span.end),
                DiagnosticSeverity::ERROR,
                Some(ntsc_diag::codes::RESOLVE),
                &message,
                None,
            ));
        }
        for error in &analysis.type_errors {
            if let Some(name) = undefined_name_of(&error.message)
                && self.workspace_knows(document, name)
            {
                continue;
            }
            let message = match &error.help {
                Some(help) => format!("{}\nhelp: {help}", error.message),
                None => error.message.clone(),
            };
            diagnostics.push(lsp_diagnostic(
                document.range(error.span.start, error.span.end),
                DiagnosticSeverity::ERROR,
                error.code.or(Some(ntsc_diag::codes::TYPE)),
                &message,
                None,
            ));
        }
        for warning in &analysis.warnings {
            diagnostics.push(lsp_diagnostic(
                document.range(warning.span.start, warning.span.end),
                DiagnosticSeverity::WARNING,
                Some(ntsc_diag::codes::WARNING),
                &warning.message,
                None,
            ));
        }
        let params = PublishDiagnosticsParams {
            uri: document.uri.clone(),
            diagnostics,
            version: Some(document.version),
        };
        if let Ok(params) = serde_json::to_value(params) {
            let _ = self.sender.send(Message::Notification(Notification {
                method: "textDocument/publishDiagnostics".to_string(),
                params,
            }));
        }
    }

    // ── Hover ──

    fn binding_at<'b>(
        &'b self,
        document: &'b Document,
        name: &str,
        offset: usize,
    ) -> Option<(DocView<'b>, &'b Symbol)> {
        let mut views = self.relevant_views(document);
        let current = views.iter().position(|view| view.uri == &document.uri)?;
        views[current] = DocView {
            uri: &document.uri,
            text: &document.text,
            analysis: &document.analysis,
            semantic: &document.semantic,
            index: &document.index,
        };
        let context = self.semantic_context(&views, current);
        let (owner, symbol) = context.named(name, offset)?;
        Some((views[owner], symbol))
    }

    fn rust_module_for(
        &self,
        document: &Document,
        qualifier: &str,
        offset: usize,
    ) -> Option<&crate::metadata::RustModule> {
        let name = qualifier.trim_start_matches('@');
        if !qualifier.starts_with('@')
            && self
                .binding_at(document, name, offset)
                .is_some_and(|(_, symbol)| symbol.kind != SymbolKind::Module)
        {
            return None;
        }
        let path = document.uri.to_file_path().ok()?;
        let root = project_root(&path);
        let mut imported = None;
        for view in self.relevant_views(document) {
            let analysis = if view.uri == &document.uri {
                &document.analysis
            } else {
                view.analysis
            };
            let Some(program) = &analysis.program else {
                continue;
            };
            for statement in &program.statements {
                if let ntsc_ast::stmt::Stmt::Use {
                    library,
                    alias,
                    is_rust: true,
                    ..
                } = statement
                {
                    if alias
                        .as_ref()
                        .unwrap_or(library)
                        .lexeme()
                        .trim_start_matches('@')
                        != name
                    {
                        continue;
                    }
                    let module = self.metadata.module(&root, library.lexeme())?;
                    if imported.is_some_and(|previous: &crate::metadata::RustModule| {
                        previous.module != module.module
                    }) {
                        return None;
                    }
                    imported = Some(module);
                }
            }
        }
        imported
    }

    fn rust_function_at(
        &self,
        document: &Document,
        offset: usize,
    ) -> Option<(&crate::metadata::RustModule, &crate::metadata::RustFunction)> {
        let (_, span, member) = document.analysis.identifier_at(offset)?;
        if !member {
            return None;
        }
        let prefix = document.text.get(..span.end)?;
        let (qualifier, name) = member_context(prefix)?;
        let module = self.rust_module_for(document, qualifier, offset)?;
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name && function.callable())?;
        Some((module, function))
    }

    fn hover(&self, params: lsp_types::HoverParams) -> Option<Hover> {
        let position = params.text_document_position_params;
        let document = self.document_for(&position.text_document.uri)?;
        let offset = document.offset(position.position)?;
        let contents = if let Some((module, function)) = self.rust_function_at(document, offset) {
            format!(
                "```ntsc\n{}\n```\n\n{}",
                function.signature(&module.module),
                function.doc
            )
        } else if let Some((_, symbol)) = self.resolved_symbol_at(document, offset) {
            if symbol.kind == SymbolKind::Module {
                stdlib::module(symbol.name.trim_start_matches('@'))
                    .map(|module| module.description.to_string())
                    .unwrap_or_else(|| symbol_markdown(symbol))
            } else {
                symbol_markdown(symbol)
            }
        } else if let Some((name, span, true)) = document.analysis.identifier_at(offset) {
            let (qualifier, _) = member_context(document.text.get(..span.end)?)?;
            let (_, binding) = self.binding_at(document, qualifier, offset)?;
            if binding.kind != SymbolKind::Module {
                return None;
            }
            let module = stdlib::module(qualifier)?;
            let function = module
                .functions
                .iter()
                .find(|function| function.name == name)?;
            format!("```ntsc\n{}\n```", function.detail)
        } else {
            let keyword = document.analysis.keyword_at(offset)?;
            builtin_markdown(keyword).or_else(|| keyword_markdown(keyword))?
        };
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: contents,
            }),
            range: None,
        })
    }

    // ── Completion ──

    fn completion(&self, params: lsp_types::CompletionParams) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let document = self.document_for(uri)?;
        let offset = document.offset(params.text_document_position.position)?;
        if !completion_code_position(document, offset) {
            return Some(CompletionResponse::Array(Vec::new()));
        }
        let recovered = completion_document(document, offset);
        let document = recovered.as_ref().unwrap_or(document);
        let line_start = document.text[..offset.min(document.text.len())]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let prefix = document
            .text
            .get(line_start..offset.min(document.text.len()))?;
        let mut items = Vec::new();
        if let Some((qualifier, partial)) = member_context(prefix) {
            items.extend(self.member_completions(document, qualifier, partial, offset));
            return Some(CompletionResponse::Array(items));
        }
        if is_use_context(prefix) {
            if let Ok(path) = document.uri.to_file_path() {
                for module in self.metadata.modules(&project_root(&path)) {
                    items.push(CompletionItem {
                        label: format!("@{}", module.module),
                        kind: Some(CompletionItemKind::MODULE),
                        detail: Some("Cached Rust adaptor".to_string()),
                        ..CompletionItem::default()
                    });
                }
            }
            for module in stdlib::MODULES {
                items.push(CompletionItem {
                    label: module.name.to_string(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(module.description.to_string()),
                    sort_text: Some(format!("0{}", module.name)),
                    ..CompletionItem::default()
                });
            }
            return Some(CompletionResponse::Array(items));
        }

        let partial = trailing_identifier(prefix);
        let expression_context = in_expression_context(prefix, partial);
        let rank = |tier: u8, label: &str| -> String { format!("{tier}{label}") };
        let matches = |label: &str| -> Option<u8> {
            // 0 = prefix match (best), 1 = camel/word match, 2 = substring.
            if partial.is_empty() {
                return Some(0);
            }
            if label.starts_with(partial) {
                return Some(0);
            }
            let lower_label = label.to_lowercase();
            let lower_partial = partial.to_lowercase();
            if lower_label.starts_with(&lower_partial) {
                return Some(0);
            }
            if lower_label.contains(&lower_partial) {
                return Some(2);
            }
            None
        };

        // Locals and parameters first: they are what the user is writing.
        for symbol in document.analysis.visible_symbols_at(offset) {
            if matches!(symbol.kind, SymbolKind::Test | SymbolKind::EnumMember) {
                continue;
            }
            let Some(tier) = matches(&symbol.name) else {
                continue;
            };
            let proximity = symbol_relevance(symbol, offset);
            items.push(CompletionItem {
                label: symbol.name.clone(),
                kind: Some(completion_kind(symbol.kind)),
                detail: Some(symbol.detail.clone()),
                documentation: documentation_of(&symbol.docs),
                sort_text: Some(format!("{proximity}{tier}{}", symbol.name)),
                filter_text: Some(symbol.name.clone()),
                ..CompletionItem::default()
            });
        }

        // Declaration keywords are noise in the middle of an expression
        // (`say(fo|)` should not offer `for`).
        if !expression_context {
            for (keyword, description) in stdlib::KEYWORDS {
                if matches(keyword).is_none() {
                    continue;
                }
                items.push(CompletionItem {
                    label: (*keyword).to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    detail: Some((*description).to_string()),
                    sort_text: Some(rank(6, keyword)),
                    ..CompletionItem::default()
                });
            }
            if self.snippets_supported {
                items.extend(snippet_completions(partial));
            }
        }

        for (builtin, detail) in stdlib::BUILTINS {
            if matches(builtin).is_none() {
                continue;
            }
            items.push(CompletionItem {
                label: (*builtin).to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some((*detail).to_string()),
                sort_text: Some(rank(4, builtin)),
                ..CompletionItem::default()
            });
        }
        for module in stdlib::MODULES {
            if !self
                .binding_at(document, module.name, offset)
                .is_some_and(|(_, symbol)| symbol.kind == SymbolKind::Module)
            {
                continue;
            }
            if matches(module.name).is_none() {
                continue;
            }
            items.push(CompletionItem {
                label: module.name.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(module.description.to_string()),
                sort_text: Some(rank(5, module.name)),
                ..CompletionItem::default()
            });
        }

        // Names from other files in the flat namespace: these are exactly
        // the cross-module symbols that single-file analysis misses.
        for (name, kind, detail, docs) in self.workspace_symbols_for(document, offset) {
            if matches(&name).is_none() {
                continue;
            }
            if items.iter().any(|item| item.label == name) {
                continue;
            }
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(completion_kind(kind)),
                detail: Some(detail),
                documentation: documentation_of(&docs),
                sort_text: Some(rank(3, &name)),
                filter_text: Some(name),
                ..CompletionItem::default()
            });
        }

        items.sort_by(|left, right| {
            left.sort_text
                .cmp(&right.sort_text)
                .then_with(|| left.label.cmp(&right.label))
        });
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(item.label.clone()));
        Some(CompletionResponse::Array(items))
    }

    /// Namespace-visible symbols from other files in the import closure.
    fn workspace_symbols_for(
        &self,
        current: &Document,
        _offset: usize,
    ) -> Vec<(String, SymbolKind, String, String)> {
        let mut found: Vec<(String, SymbolKind, String, String)> = Vec::new();
        let views = self.relevant_views(current);
        let current_view = views
            .iter()
            .position(|view| view.uri == &current.uri)
            .unwrap_or(0);
        let context = self.semantic_context(&views, current_view);
        for (index, view) in views.iter().enumerate() {
            if view.uri == &current.uri {
                continue;
            }
            if context.namespaced_files.contains(&index) {
                continue;
            }
            for symbol in &view.analysis.symbols {
                if !is_cross_file(symbol)
                    || view
                        .semantic
                        .private_symbols
                        .contains(&symbol.name_span.start)
                {
                    continue;
                }
                if found.iter().any(|(name, ..)| *name == symbol.name) {
                    continue;
                }
                found.push((
                    symbol.name.clone(),
                    symbol.kind,
                    symbol.detail.clone(),
                    symbol.docs.clone(),
                ));
            }
        }
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found
    }

    fn member_completions(
        &self,
        document: &Document,
        qualifier: &str,
        partial: &str,
        offset: usize,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        if let Some(module) = self.rust_module_for(document, qualifier, offset) {
            for function in &module.functions {
                if !function.callable() || !function.name.starts_with(partial) {
                    continue;
                }
                items.push(CompletionItem {
                    label: function.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(function.signature(&module.module)),
                    documentation: documentation_of(&function.doc),
                    ..CompletionItem::default()
                });
            }
            return items;
        }
        let qualifier = qualifier.trim_start_matches('@');
        let resolved_binding = self.binding_at(document, qualifier, offset);
        let binding = resolved_binding.as_ref().map(|(_, symbol)| *symbol);
        if binding.is_some_and(|symbol| symbol.kind == SymbolKind::Module) {
            let Some((view, _)) = resolved_binding.as_ref() else {
                return items;
            };
            if let Some(namespace) = self.native_namespace(*view, qualifier) {
                for symbol in &namespace.analysis.symbols {
                    if is_cross_file(symbol)
                        && symbol.name.starts_with(partial)
                        && !namespace
                            .semantic
                            .private_symbols
                            .contains(&symbol.name_span.start)
                    {
                        items.push(CompletionItem {
                            label: symbol.name.clone(),
                            kind: Some(completion_kind(symbol.kind)),
                            detail: Some(symbol.detail.clone()),
                            documentation: documentation_of(&symbol.docs),
                            ..CompletionItem::default()
                        });
                    }
                }
                return items;
            }
        }
        if binding.is_some_and(|symbol| symbol.kind == SymbolKind::Module)
            && let Some(module) = stdlib::module(qualifier)
        {
            for function in module.functions {
                if !partial.is_empty() && !function.name.starts_with(partial) {
                    continue;
                }
                items.push(CompletionItem {
                    label: function.name.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some(function.detail.to_string()),
                    ..CompletionItem::default()
                });
            }
            return items;
        }
        let views = self.relevant_views(document);
        let Some(current) = views.iter().position(|view| view.uri == &document.uri) else {
            return items;
        };
        let mut context = self.semantic_context(&views, current);
        context.views = views
            .iter()
            .map(|view| {
                if view.uri == &document.uri {
                    SemanticView {
                        analysis: &document.analysis,
                        index: &document.semantic,
                    }
                } else {
                    SemanticView {
                        analysis: view.analysis,
                        index: view.semantic,
                    }
                }
            })
            .collect();
        let receiver_end = offset.saturating_sub(partial.len());
        let Some(base) = document
            .text
            .get(..receiver_end)
            .and_then(|text| text.rfind(qualifier))
        else {
            return items;
        };
        let Some(source) = ValueSource::parse(qualifier, base) else {
            return items;
        };
        for (_, symbol) in context.members(&source) {
            if !partial.is_empty() && !symbol.name.starts_with(partial) {
                continue;
            }
            items.push(CompletionItem {
                label: symbol.name.clone(),
                kind: Some(completion_kind(symbol.kind)),
                detail: Some(symbol.detail.clone()),
                ..CompletionItem::default()
            });
        }
        items
    }

    // ── Definition / references / highlight ──

    fn resolved_symbol_for_view<'b>(
        &'b self,
        document: &'b Document,
        view: DocView<'b>,
        offset: usize,
    ) -> Option<(DocView<'b>, &'b Symbol)> {
        let mut views = self.relevant_views(document);
        let current = views
            .iter()
            .position(|candidate| candidate.uri == view.uri)?;
        if view.uri == &document.uri {
            views[current] = view;
        }
        let context = self.semantic_context(&views, current);
        let (name, _, member) = view.analysis.identifier_at(offset)?;
        let reference = view.analysis.references.iter().find(|reference| {
            reference.name == name && reference.span.start <= offset && offset <= reference.span.end
        });
        let (owner, symbol) =
            if member && reference.is_some_and(|reference| !reference.is_definition) {
                context.member(reference?.receiver_source.as_ref()?, name)
            } else if let Some(target) = reference.and_then(|reference| reference.target) {
                view.analysis
                    .symbols
                    .get(target)
                    .map(|symbol| (current, symbol))
            } else {
                context.named(name, offset)
            }?;
        Some((views[owner], symbol))
    }

    fn resolved_symbol_at<'b>(
        &'b self,
        document: &'b Document,
        offset: usize,
    ) -> Option<(DocView<'b>, &'b Symbol)> {
        let view = DocView {
            uri: &document.uri,
            text: &document.text,
            analysis: &document.analysis,
            semantic: &document.semantic,
            index: &document.index,
        };
        self.resolved_symbol_for_view(document, view, offset)
    }

    fn goto_definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Option<lsp_types::GotoDefinitionResponse> {
        let position = params.text_document_position_params;
        let document = self.document_for(&position.text_document.uri)?;
        let offset = document.offset(position.position)?;
        let (view, symbol) = self.resolved_symbol_at(document, offset)?;
        Some(lsp_types::GotoDefinitionResponse::Scalar(Location::new(
            view.uri.clone(),
            span_range(&view, symbol.name_span),
        )))
    }

    fn symbol_index_at(&self, document: &Document, offset: usize) -> Option<usize> {
        let (view, symbol) = self.resolved_symbol_at(document, offset)?;
        if view.uri != &document.uri {
            return None;
        }
        document
            .analysis
            .symbols
            .iter()
            .position(|candidate| candidate.name_span == symbol.name_span)
    }

    fn symbol_references(
        &self,
        document: &Document,
        offset: usize,
        declaration: bool,
    ) -> Option<Vec<Location>> {
        let (target_view, target) = self.resolved_symbol_at(document, offset)?;
        let mut locations = Vec::new();
        if declaration {
            locations.push(Location::new(
                target_view.uri.clone(),
                span_range(&target_view, target.name_span),
            ));
        }
        for view in self.relevant_views(document) {
            for reference in &view.analysis.references {
                if reference.is_definition || reference.name != target.name {
                    continue;
                }
                if let Some((owner, symbol)) =
                    self.resolved_symbol_for_view(document, view, reference.span.start)
                    && owner.uri == target_view.uri
                    && symbol.name_span == target.name_span
                {
                    locations.push(Location::new(
                        view.uri.clone(),
                        span_range(&view, reference.span),
                    ));
                }
            }
        }
        locations.sort_by(|left, right| {
            left.uri
                .as_str()
                .cmp(right.uri.as_str())
                .then_with(|| position_key(left.range.start).cmp(&position_key(right.range.start)))
        });
        locations.dedup();
        Some(locations)
    }

    fn find_references(&self, params: lsp_types::ReferenceParams) -> Option<Vec<Location>> {
        let position = params.text_document_position;
        let document = self.document_for(&position.text_document.uri)?;
        let offset = document.offset(position.position)?;
        self.symbol_references(document, offset, params.context.include_declaration)
    }

    fn document_highlight(
        &self,
        params: lsp_types::DocumentHighlightParams,
    ) -> Option<Vec<DocumentHighlight>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let document = self.document_for(uri)?;
        let offset = document.offset(params.text_document_position_params.position)?;
        let symbol_idx = self.symbol_index_at(document, offset)?;
        let declaration = document
            .analysis
            .symbols
            .get(symbol_idx)
            .map(|symbol| document.range(symbol.name_span.start, symbol.name_span.end));
        let highlights = document
            .analysis
            .references_to(symbol_idx)
            .into_iter()
            .map(|span| {
                let range = document.range(span.start, span.end);
                let kind = if Some(range) == declaration {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                };
                DocumentHighlight {
                    range,
                    kind: Some(kind),
                }
            })
            .collect();
        Some(highlights)
    }

    // ── Rename ──

    fn prepare_rename(
        &self,
        params: lsp_types::TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let document = self.document_for(&params.text_document.uri)?;
        let offset = document.offset(params.position)?;
        let (name, span, _) = document.analysis.identifier_at(offset)?;
        self.resolved_symbol_at(document, offset)?;
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: document.range(span.start, span.end),
            placeholder: name.to_string(),
        })
    }

    fn rename(&self, id: RequestId, params: lsp_types::RenameParams) -> Response {
        let position = params.text_document_position;
        let Some(document) = self.document_for(&position.text_document.uri) else {
            return error_response(id, "document is not open");
        };
        let Some(offset) = document.offset(position.position) else {
            return error_response(id, "position is out of range");
        };
        if !is_valid_name(&params.new_name) {
            return error_response(id, "invalid Neutron identifier");
        }
        let Some((owner, target)) = self.resolved_symbol_at(document, offset) else {
            return error_response(id, "no unambiguous symbol at position");
        };
        if target.kind == SymbolKind::Method && target.name == "init" {
            return error_response(id, "constructors cannot be renamed as ordinary methods");
        }
        if target.kind == SymbolKind::Module
            && !owner.analysis.program.as_ref().is_some_and(|program| program.statements.iter().any(|statement| {
                matches!(statement, ntsc_ast::stmt::Stmt::Use { alias: Some(alias), .. } if alias.span == target.name_span)
            }))
        {
            return error_response(id, "rename a module alias, not a dependency name");
        }
        for view in self.relevant_views(document) {
            if view.analysis.symbols.iter().any(|symbol| {
                symbol.name == params.new_name
                    && (view.uri != owner.uri || symbol.name_span != target.name_span)
                    && ((is_cross_file(target) && is_cross_file(symbol))
                        || (view.uri == owner.uri
                            && symbol.container == target.container
                            && symbol.scope_range.0 <= target.scope_range.1
                            && target.scope_range.0 <= symbol.scope_range.1))
            }) {
                return error_response(id, "the new name conflicts with an existing declaration");
            }
        }
        let Some(locations) = self.symbol_references(document, offset, true) else {
            return error_response(id, "no unambiguous symbol at position");
        };
        let mut changes: HashMap<Url, Vec<lsp_types::TextEdit>> = HashMap::new();
        for location in locations {
            let preserve_at = self
                .all_views()
                .into_iter()
                .find(|view| view.uri == &location.uri)
                .and_then(|view| {
                    view.index
                        .offset(view.text, location.range.start)
                        .map(|offset| (view, offset))
                })
                .is_some_and(|(view, offset)| {
                    view.text
                        .get(offset..)
                        .is_some_and(|text| text.starts_with('@'))
                });
            changes
                .entry(location.uri)
                .or_default()
                .push(lsp_types::TextEdit {
                    range: location.range,
                    new_text: if preserve_at {
                        format!("@{}", params.new_name)
                    } else {
                        params.new_name.clone()
                    },
                });
        }
        Response::new_ok(
            id,
            WorkspaceEdit {
                changes: Some(changes),
                ..WorkspaceEdit::default()
            },
        )
    }

    // ── Symbols ──

    fn document_symbols(
        &self,
        params: lsp_types::DocumentSymbolParams,
    ) -> Option<Vec<DocumentSymbol>> {
        let document = self.document_for(&params.text_document.uri)?;
        let analysis = &document.analysis;
        let mut children: Vec<Vec<DocumentSymbol>> = vec![Vec::new(); analysis.symbols.len()];
        let mut roots = Vec::new();
        for (idx, symbol) in analysis.symbols.iter().enumerate() {
            let full_range = symbol_full_range(&document.index, &document.text, symbol);
            #[allow(deprecated)]
            let entry = DocumentSymbol {
                name: symbol.name.clone(),
                detail: Some(symbol.detail.clone()),
                kind: lsp_symbol_kind(symbol.kind),
                tags: None,
                deprecated: None,
                range: full_range,
                selection_range: document.range(symbol.name_span.start, symbol.name_span.end),
                children: None,
            };
            if let Some(parent) = symbol.parent
                && parent < children.len()
            {
                children[parent].push(entry);
                continue;
            }
            roots.push((idx, entry));
        }
        let mut result = Vec::with_capacity(roots.len());
        for (idx, mut entry) in roots {
            let nested = std::mem::take(&mut children[idx]);
            if !nested.is_empty() {
                entry.children = Some(nested);
            }
            result.push(entry);
        }
        Some(result)
    }

    fn workspace_symbols(
        &self,
        params: lsp_types::WorkspaceSymbolParams,
    ) -> Option<Vec<SymbolInformation>> {
        let query = params.query.to_lowercase();
        let mut results = Vec::new();
        for view in self.all_views() {
            for symbol in &view.analysis.symbols {
                if !query.is_empty() && !symbol.name.to_lowercase().contains(&query) {
                    continue;
                }
                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: symbol.name.clone(),
                    kind: lsp_symbol_kind(symbol.kind),
                    tags: None,
                    deprecated: None,
                    location: Location::new(view.uri.clone(), span_range(&view, symbol.name_span)),
                    container_name: symbol.container.clone(),
                });
                if results.len() >= 100 {
                    return Some(results);
                }
            }
        }
        Some(results)
    }

    // ── Signature help ──

    fn signature_help(&self, params: lsp_types::SignatureHelpParams) -> Option<SignatureHelp> {
        let position = params.text_document_position_params;
        let original = self.document_for(&position.text_document.uri)?;
        let offset = original.offset(position.position)?;
        let recovered = completion_document(original, offset);
        let document = recovered.as_ref().unwrap_or(original);
        let call = document.analysis.enclosing_call(offset)?;
        let callee_end = call.paren_span.start;
        let prefix = document.text.get(..callee_end)?.trim_end();
        let active = active_parameter(&document.text, call, offset);
        let (label, documentation, parameters) = if let Some((qualifier, name)) =
            member_context(prefix)
        {
            if let Some(module) = self.rust_module_for(document, qualifier, offset) {
                let function = module
                    .functions
                    .iter()
                    .find(|function| function.name == name && function.callable())?;
                let parameters = function
                    .params
                    .iter()
                    .map(|parameter| ParameterInformation {
                        label: lsp_types::ParameterLabel::Simple(format!(
                            "{}: {}",
                            parameter.name, parameter.label
                        )),
                        documentation: None,
                    })
                    .collect();
                (
                    function.signature(&module.module),
                    documentation_of(&function.doc),
                    parameters,
                )
            } else {
                let (_, symbol) =
                    self.resolved_symbol_at(document, prefix.len().saturating_sub(1))?;
                let parameters = symbol
                    .params
                    .iter()
                    .map(|(name, ty)| ParameterInformation {
                        label: lsp_types::ParameterLabel::Simple(format!("{name}: {ty}")),
                        documentation: None,
                    })
                    .collect();
                (
                    symbol.detail.clone(),
                    documentation_of(&symbol.docs),
                    parameters,
                )
            }
        } else {
            let name = call.callee_name.as_deref()?;
            let (_, symbol) = self.binding_at(document, name, prefix.len().saturating_sub(1))?;
            if !matches!(
                symbol.kind,
                SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Method
            ) {
                return None;
            }
            let parameters = symbol
                .params
                .iter()
                .map(|(name, ty)| ParameterInformation {
                    label: lsp_types::ParameterLabel::Simple(format!("{name}: {ty}")),
                    documentation: None,
                })
                .collect();
            (
                symbol.detail.clone(),
                documentation_of(&symbol.docs),
                parameters,
            )
        };
        Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation,
                parameters: Some(parameters),
                active_parameter: Some(active),
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        })
    }

    // ── Formatting ──

    fn formatting(
        &self,
        params: lsp_types::DocumentFormattingParams,
    ) -> Option<Vec<lsp_types::TextEdit>> {
        let document = self.document_for(&params.text_document.uri)?;
        let formatted = crate::formatting::format_document(&document.text)?;
        let end_position = document.index.position(&document.text, document.text.len());
        Some(vec![lsp_types::TextEdit {
            range: Range::new(lsp_types::Position::new(0, 0), end_position),
            new_text: formatted,
        }])
    }

    // ── Folding ──

    fn folding_ranges(&self, params: lsp_types::FoldingRangeParams) -> Option<Vec<FoldingRange>> {
        let document = self.document_for(&params.text_document.uri)?;
        let mut stack = Vec::new();
        let mut ranges = Vec::new();
        for token in &document.analysis.tokens {
            match token.kind {
                ntsc_ast::token::TokenKind::LeftBrace => {
                    stack.push(document.index.line_for_offset(token.span.start));
                }
                ntsc_ast::token::TokenKind::RightBrace => {
                    if let Some(start_line) = stack.pop() {
                        let end_line = document.index.line_for_offset(token.span.start);
                        if end_line > start_line {
                            ranges.push(FoldingRange {
                                start_line: start_line as u32,
                                start_character: None,
                                end_line: end_line as u32,
                                end_character: None,
                                kind: Some(FoldingRangeKind::Region),
                                collapsed_text: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Some(ranges)
    }

    // ── Semantic tokens ──

    fn semantic_tokens(
        &self,
        params: lsp_types::SemanticTokensParams,
    ) -> Option<SemanticTokensResult> {
        let document = self.document_for(&params.text_document.uri)?;
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic_token_data(document),
        }))
    }

    // ── Inlay hints ──

    fn inlay_hints(&self, params: lsp_types::InlayHintParams) -> Option<Vec<InlayHint>> {
        let document = self.document_for(&params.text_document.uri)?;
        let mut hints = Vec::new();
        for call in &document.analysis.calls {
            let Some(name) = call.callee_name.as_deref() else {
                continue;
            };
            let probe = call.paren_span.start.saturating_add(1);
            let Some(symbol) = document.analysis.callable_at(name, probe) else {
                continue;
            };
            if symbol.params.is_empty() {
                continue;
            }
            for (arg, (param, _)) in call.arg_spans.iter().zip(symbol.params.iter()) {
                if !is_realistic_span(*arg) {
                    continue;
                }
                let position = document.index.position(&document.text, arg.start);
                if position < params.range.start || position > params.range.end {
                    continue;
                }
                hints.push(InlayHint {
                    position,
                    label: InlayHintLabel::String(format!("{param}:")),
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: Some(true),
                    data: None,
                });
            }
        }
        Some(hints)
    }

    // ── Code actions ──

    fn code_actions(
        &self,
        params: lsp_types::CodeActionParams,
    ) -> Option<Vec<lsp_types::CodeActionOrCommand>> {
        use lsp_types::CodeActionOrCommand;
        let document = self.document_for(&params.text_document.uri)?;
        let mut actions = Vec::new();
        for error in &document.analysis.resolve_errors {
            let Some(suggestion) = error.suggestion.as_deref() else {
                continue;
            };
            let range = document.range(error.span.start, error.span.end);
            if !ranges_overlap(range, params.range) {
                continue;
            }
            let uri = params.text_document.uri.clone();
            let edit = WorkspaceEdit {
                changes: Some(
                    [(
                        uri,
                        vec![lsp_types::TextEdit {
                            range,
                            new_text: suggestion.to_string(),
                        }],
                    )]
                    .into_iter()
                    .collect(),
                ),
                ..WorkspaceEdit::default()
            };
            actions.push(CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
                title: format!("Replace with `{suggestion}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(edit),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }
        Some(actions)
    }

    // ── Selection ranges ──

    fn selection_ranges(
        &self,
        params: lsp_types::SelectionRangeParams,
    ) -> Option<Vec<SelectionRange>> {
        let document = self.document_for(&params.text_document.uri)?;
        let mut result = Vec::with_capacity(params.positions.len());
        for position in &params.positions {
            let Some(offset) = document.offset(*position) else {
                result.push(SelectionRange {
                    range: Range::new(*position, *position),
                    parent: None,
                });
                continue;
            };
            let mut current: Option<Box<SelectionRange>> = None;
            for (start, end) in document.analysis.selection_chain(offset).into_iter().rev() {
                current = Some(Box::new(SelectionRange {
                    range: document.range(start, end),
                    parent: current,
                }));
            }
            result.push(current.map_or(
                SelectionRange {
                    range: document.range(offset, offset),
                    parent: None,
                },
                |boxed| *boxed,
            ));
        }
        Some(result)
    }

    // ── Document links ──

    fn document_links(&self, params: lsp_types::DocumentLinkParams) -> Option<Vec<DocumentLink>> {
        let document = self.document_for(&params.text_document.uri)?;
        let base = document.uri.to_file_path().ok()?;
        let base = base.parent()?.to_path_buf();
        let program = document.analysis.program.as_ref()?;
        let mut links = Vec::new();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                is_file_path: true,
                ..
            } = stmt
            {
                let import = library.lexeme();
                if import.is_empty() {
                    continue;
                }
                let mut candidate = base.join(import);
                if candidate.extension().is_none() {
                    candidate.set_extension("nt");
                }
                let Ok(target) = Url::from_file_path(normalize_path(&candidate)) else {
                    continue;
                };
                links.push(DocumentLink {
                    range: document.range(library.span.start, library.span.end),
                    target: Some(target),
                    tooltip: None,
                    data: None,
                });
            }
        }
        Some(links)
    }

    // ── Call hierarchy ──

    fn hierarchy_item(view: &DocView<'_>, symbol: &Symbol) -> CallHierarchyItem {
        CallHierarchyItem {
            name: symbol.name.clone(),
            kind: lsp_symbol_kind(symbol.kind),
            tags: None,
            detail: Some(symbol.detail.clone()),
            uri: view.uri.clone(),
            range: symbol_full_range(view.index, view.text, symbol),
            selection_range: span_range(view, symbol.name_span),
            data: None,
        }
    }

    fn prepare_call_hierarchy(
        &self,
        params: lsp_types::CallHierarchyPrepareParams,
    ) -> Option<Vec<CallHierarchyItem>> {
        let position = params.text_document_position_params;
        let document = self.document_for(&position.text_document.uri)?;
        let offset = document.offset(position.position)?;
        let view = DocView {
            uri: &document.uri,
            text: &document.text,
            analysis: &document.analysis,
            semantic: &document.semantic,
            index: &document.index,
        };
        // On an unresolved call site, jump straight to the callee wherever
        // it is defined — that is the interesting hierarchy root, not the
        // function that happens to contain the call.
        if let Some((name, _, false)) = document.analysis.identifier_at(offset) {
            let unresolved = document.analysis.references.iter().any(|reference| {
                reference.name == name
                    && !reference.is_definition
                    && reference.target.is_none()
                    && reference.span.start <= offset
                    && offset <= reference.span.end
            });
            if unresolved && let Some((target_view, symbol)) = self.find_callable(name) {
                return Some(vec![Self::hierarchy_item(&target_view, symbol)]);
            }
            if let Some(symbol) = document.analysis.definition_at(offset)
                && matches!(
                    symbol.kind,
                    SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Method
                )
            {
                return Some(vec![Self::hierarchy_item(&view, symbol)]);
            }
        }
        // Otherwise any position inside a body prepares that body.
        let function = document.analysis.enclosing_function(offset)?;
        Some(vec![Self::hierarchy_item(&view, function)])
    }

    fn incoming_calls(
        &self,
        params: lsp_types::CallHierarchyIncomingCallsParams,
    ) -> Option<Vec<CallHierarchyIncomingCall>> {
        let name = params.item.name.clone();
        let mut groups: HashMap<(String, String), (CallHierarchyItem, Vec<Range>)> = HashMap::new();
        for view in self.all_views() {
            for call in &view.analysis.calls {
                if call.callee_name.as_deref() != Some(name.as_str()) {
                    continue;
                }
                let Some(caller) = view.analysis.enclosing_function(call.paren_span.start) else {
                    continue;
                };
                let range = span_range(&view, call.full_span);
                groups
                    .entry((view.uri.as_str().to_string(), caller.name.clone()))
                    .or_insert_with(|| (Self::hierarchy_item(&view, caller), Vec::new()))
                    .1
                    .push(range);
            }
        }
        let mut result: Vec<CallHierarchyIncomingCall> = groups
            .into_values()
            .map(|(from, mut from_ranges)| {
                sort_ranges(&mut from_ranges);
                CallHierarchyIncomingCall { from, from_ranges }
            })
            .collect();
        result.sort_by(|left, right| {
            left.from
                .uri
                .as_str()
                .cmp(right.from.uri.as_str())
                .then(left.from.name.cmp(&right.from.name))
        });
        Some(result)
    }

    fn outgoing_calls(
        &self,
        params: lsp_types::CallHierarchyOutgoingCallsParams,
    ) -> Option<Vec<CallHierarchyOutgoingCall>> {
        let item = &params.item;
        let mut groups: HashMap<(String, String), (CallHierarchyItem, Vec<Range>)> = HashMap::new();
        for caller_view in self.all_views() {
            if caller_view.uri != &item.uri {
                continue;
            }
            let Some(caller) = caller_view
                .analysis
                .symbols
                .iter()
                .find(|symbol| symbol.name == item.name && is_callable(symbol))
            else {
                continue;
            };
            let Some((start, end)) = caller.body_range else {
                continue;
            };
            for call in &caller_view.analysis.calls {
                if !(start <= call.paren_span.start && call.paren_span.start <= end) {
                    continue;
                }
                let Some(callee_name) = call.callee_name.as_deref() else {
                    continue;
                };
                let Some((target_view, target)) = self.find_callable(callee_name) else {
                    continue;
                };
                let range = span_range(&caller_view, call.full_span);
                groups
                    .entry((target_view.uri.as_str().to_string(), target.name.clone()))
                    .or_insert_with(|| (Self::hierarchy_item(&target_view, target), Vec::new()))
                    .1
                    .push(range);
            }
        }
        let mut result: Vec<CallHierarchyOutgoingCall> = groups
            .into_values()
            .map(|(to, mut from_ranges)| {
                sort_ranges(&mut from_ranges);
                CallHierarchyOutgoingCall { to, from_ranges }
            })
            .collect();
        result.sort_by(|left, right| {
            left.to
                .uri
                .as_str()
                .cmp(right.to.uri.as_str())
                .then(left.to.name.cmp(&right.to.name))
        });
        Some(result)
    }

    /// First callable symbol with `name` across open documents and the
    /// import closure, with the view that owns it.
    fn find_callable(&self, name: &str) -> Option<(DocView<'_>, &Symbol)> {
        for view in self.all_views() {
            if let Some(symbol) = view
                .analysis
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && is_callable(symbol))
            {
                return Some((view, symbol));
            }
        }
        None
    }

    // ── Workspace commands ──

    fn execute_command(&self, id: RequestId, params: lsp_types::ExecuteCommandParams) -> Response {
        if params.command != LIST_TESTS_COMMAND {
            return error_response(id, "unknown command");
        }
        let mut tests = Vec::new();
        for view in self.all_views() {
            for symbol in &view.analysis.symbols {
                if symbol.kind != SymbolKind::Test {
                    continue;
                }
                tests.push(serde_json::json!({
                    "name": symbol.name,
                    "uri": view.uri,
                    "range": span_range(&view, symbol.name_span),
                }));
            }
        }
        Response::new_ok(id, tests)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Cap on disk-loaded import-closure files: the server re-reads them on
/// every keystroke batch, so this bounds both memory and latency.
const MAX_WORKSPACE_FILES: usize = 256;
/// Files larger than this are never pulled into the import closure.
const MAX_WORKSPACE_FILE_BYTES: usize = 4_000_000;

/// File paths imported via `use "path"` in top-level statements.
fn file_imports_of(analysis: &FileAnalysis, directory: &Path) -> Vec<String> {
    let mut imports = Vec::new();
    if let Some(program) = analysis.program.as_ref() {
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                is_file_path,
                is_rust: false,
                ..
            } = stmt
            {
                let path = if *is_file_path {
                    library.lexeme().to_string()
                } else {
                    let root = project_root(&directory.join("_"));
                    let Some(path) = ntsc_build::modules::vendored_import(&root, library.lexeme())
                    else {
                        continue;
                    };
                    path.to_string_lossy().into_owned()
                };
                if !path.is_empty() && !imports.contains(&path) {
                    imports.push(path);
                }
            }
        }
    }
    imports
}

/// Lexically normalize a path (resolve `.`/`..`) without touching the
/// filesystem, so missing files can still be compared by identity.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(component.as_os_str()),
        }
    }
    out
}

fn span_range(view: &DocView<'_>, span: ntsc_ast::span::Span) -> Range {
    view.index.range(view.text, span.start, span.end)
}

fn symbol_full_range(index: &LineIndex, text: &str, symbol: &Symbol) -> Range {
    if let Some((start, end)) = symbol.body_range {
        index.range(
            text,
            start.min(symbol.name_span.start),
            end.max(symbol.name_span.end),
        )
    } else {
        full_symbol_range_legacy(index, text, symbol)
    }
}

/// Whether a symbol defined in one file can be referenced from another:
/// Neutron merges modules into a single flat namespace, so only locals
/// (variables, parameters) are file-private.
fn is_cross_file(symbol: &Symbol) -> bool {
    if symbol.parent.is_some()
        || symbol.container.is_some()
        || matches!(
            symbol.kind,
            SymbolKind::Method | SymbolKind::Field | SymbolKind::EnumMember | SymbolKind::Test
        )
    {
        return false;
    }
    let is_local = matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter);
    if !is_local {
        return true;
    }
    // A top-level `var` is a global and crosses the file boundary; a local
    // `var` is private to its own scope.
    let (start, end) = symbol.scope_range;
    start == 0 && end == usize::MAX
}

/// Functions, methods, and tests: the navigable call targets of the
/// language.
fn is_callable(symbol: &Symbol) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Method | SymbolKind::Test
    )
}

/// The undefined name carried by a resolver "undefined name `x`" message,
/// if the message is one. Type errors wrap resolver errors verbatim, so
/// this matches both.
fn undefined_name_of(message: &str) -> Option<&str> {
    message.strip_prefix("undefined name `")?.strip_suffix('`')
}

fn error_response(id: RequestId, message: &str) -> Response {
    Response {
        id,
        result: None,
        error: Some(lsp_server::ResponseError {
            code: lsp_server::ErrorCode::InvalidParams as i32,
            message: message.to_string(),
            data: None,
        }),
    }
}

fn lsp_diagnostic(
    range: Range,
    severity: DiagnosticSeverity,
    code: Option<&str>,
    message: &str,
    data: Option<serde_json::Value>,
) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range,
        severity: Some(severity),
        code: code.map(|code| NumberOrString::String(code.to_string())),
        code_description: None,
        source: Some(DIAGNOSTIC_SOURCE.to_string()),
        message: message.to_string(),
        related_information: None,
        tags: None,
        data,
    }
}

fn completion_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Test => {
            CompletionItemKind::FUNCTION
        }
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Class => CompletionItemKind::CLASS,
        SymbolKind::Field => CompletionItemKind::FIELD,
        SymbolKind::Variable => CompletionItemKind::VARIABLE,
        SymbolKind::Parameter => CompletionItemKind::VARIABLE,
        SymbolKind::Enum => CompletionItemKind::ENUM,
        SymbolKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        SymbolKind::Trait => CompletionItemKind::INTERFACE,
        SymbolKind::TypeAlias => CompletionItemKind::CLASS,
        SymbolKind::Module => CompletionItemKind::MODULE,
    }
}

fn lsp_symbol_kind(kind: SymbolKind) -> LspSymbolKind {
    match kind {
        SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Test => {
            LspSymbolKind::FUNCTION
        }
        SymbolKind::Method => LspSymbolKind::METHOD,
        SymbolKind::Class => LspSymbolKind::CLASS,
        SymbolKind::Field => LspSymbolKind::FIELD,
        SymbolKind::Variable | SymbolKind::Parameter => LspSymbolKind::VARIABLE,
        SymbolKind::Enum => LspSymbolKind::ENUM,
        SymbolKind::EnumMember => LspSymbolKind::ENUM_MEMBER,
        SymbolKind::Trait => LspSymbolKind::INTERFACE,
        SymbolKind::TypeAlias => LspSymbolKind::CLASS,
        SymbolKind::Module => LspSymbolKind::MODULE,
    }
}

fn full_symbol_range_legacy(index: &LineIndex, text: &str, symbol: &Symbol) -> Range {
    let (start, end) = symbol.scope_range;
    if start == 0 && end == usize::MAX {
        index.range(text, symbol.name_span.start, symbol.name_span.end)
    } else {
        index.range(
            text,
            start.min(symbol.name_span.start),
            end.max(symbol.name_span.end),
        )
    }
}

/// Split a line prefix into `(qualifier, partial)` when the cursor is in a
/// member-access position (`qualifier.` or `qualifier.partial`).
fn member_context(prefix: &str) -> Option<(&str, &str)> {
    let trimmed = prefix.trim_end();
    let tokens = ntsc_lexer::tokenize(trimmed);
    let dot = tokens.iter().rposition(|token| {
        matches!(
            token.kind,
            ntsc_ast::token::TokenKind::Dot | ntsc_ast::token::TokenKind::QuestionDot
        )
    })?;
    let dot_token = &tokens[dot];
    let after_dot = trimmed.get(dot_token.span.end..)?;
    if !after_dot.is_empty()
        && !after_dot
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_')
    {
        return None;
    }
    let mut start = dot_token.span.start;
    let mut depth = 0usize;
    for token in tokens[..dot].iter().rev() {
        let lexeme = token.lexeme();
        if matches!(
            token.kind,
            ntsc_ast::token::TokenKind::RightParen
                | ntsc_ast::token::TokenKind::RightBracket
                | ntsc_ast::token::TokenKind::RightBrace
        ) {
            depth += 1;
        } else if matches!(
            token.kind,
            ntsc_ast::token::TokenKind::LeftParen
                | ntsc_ast::token::TokenKind::LeftBracket
                | ntsc_ast::token::TokenKind::LeftBrace
        ) {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if depth == 0
            && !matches!(
                token.kind,
                ntsc_ast::token::TokenKind::Identifier(_)
                    | ntsc_ast::token::TokenKind::This
                    | ntsc_ast::token::TokenKind::Super
            )
            && !matches!(
                token.kind,
                ntsc_ast::token::TokenKind::Dot | ntsc_ast::token::TokenKind::QuestionDot
            )
            && lexeme != "@"
        {
            break;
        }
        start = token.span.start;
    }
    let qualifier = trimmed.get(start..dot_token.span.start)?.trim();
    if qualifier.is_empty()
        || !qualifier
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || matches!(ch, '_' | '@' | '('))
    {
        return None;
    }
    Some((qualifier, after_dot))
}

fn completion_code_position(document: &Document, offset: usize) -> bool {
    if document.analysis.tokens.iter().any(|token| {
        token.span.start < offset
            && offset < token.span.end
            && matches!(token.kind, ntsc_ast::token::TokenKind::StringLiteral(_))
    }) {
        return false;
    }
    let Some(prefix) = document.text.get(..offset) else {
        return false;
    };
    let line_start = prefix.rfind('\n').map_or(0, |position| position + 1);
    let previous = document
        .analysis
        .tokens
        .iter()
        .filter(|token| token.span.end <= offset && token.kind != ntsc_ast::token::TokenKind::Eof)
        .map(|token| token.span.end)
        .max()
        .unwrap_or(0);
    let gap = &prefix[previous.min(offset)..];
    if gap
        .rfind("/*")
        .is_some_and(|open| gap.rfind("*/").is_none_or(|close| open > close))
    {
        return false;
    }
    let gap = &prefix[previous.max(line_start).min(offset)..];
    !gap.contains("//") && !gap.contains('#')
}

fn completion_document(document: &Document, offset: usize) -> Option<Document> {
    if document.analysis.parse_errors.is_empty() || !document.text.is_char_boundary(offset) {
        return None;
    }
    let mut text = document.text.clone();
    text.insert_str(offset, "__ntsc_completion");
    let mut endings = Vec::new();
    for token in ntsc_lexer::tokenize(&text) {
        let ending = match token.kind {
            ntsc_ast::token::TokenKind::LeftParen => {
                endings.push(')');
                None
            }
            ntsc_ast::token::TokenKind::LeftBracket => {
                endings.push(']');
                None
            }
            ntsc_ast::token::TokenKind::LeftBrace => {
                endings.push('}');
                None
            }
            ntsc_ast::token::TokenKind::RightParen => Some(')'),
            ntsc_ast::token::TokenKind::RightBracket => Some(']'),
            ntsc_ast::token::TokenKind::RightBrace => Some('}'),
            _ => None,
        };
        if let Some(ending) = ending
            && endings.last() == Some(&ending)
        {
            endings.pop();
        }
    }
    text.extend(endings.into_iter().rev());
    Some(Document::new(document.uri.clone(), document.version, text))
}

/// Markdown hover body for a builtin value: its call signature.
fn builtin_markdown(name: &str) -> Option<String> {
    stdlib::BUILTINS
        .iter()
        .find(|(label, _)| *label == name)
        .map(|(builtin, detail)| format!("```neutron\n{detail}\n```\n*builtin `{builtin}`*"))
}

/// Markdown hover body for a keyword: `` `keyword` — description ``.
fn keyword_markdown(keyword: &str) -> Option<String> {
    stdlib::KEYWORDS
        .iter()
        .find(|(label, _)| *label == keyword)
        .map(|(_, description)| format!("`{keyword}` — {description}"))
}

/// Markdown hover body for a symbol: signature, doc comment, parameters,
/// and return type.
fn symbol_markdown(symbol: &Symbol) -> String {
    let mut out = format!(
        "```neutron\n{}\n```\n*{}*{}",
        symbol.detail,
        symbol.kind.label(),
        symbol
            .container
            .as_ref()
            .map_or_else(String::new, |container| format!(" of `{container}`")),
    );
    if !symbol.docs.is_empty() {
        out.push_str("\n\n---\n\n");
        out.push_str(&symbol.docs);
    }
    if !symbol.params.is_empty() {
        out.push_str("\n\n**Parameters**\n");
        for (param, ty) in &symbol.params {
            out.push_str(&format!("- `{param}`: `{ty}`\n"));
        }
        if let Some(return_type) = &symbol.return_type {
            out.push_str(&format!("\n**Returns** `{return_type}`"));
        }
    }
    out
}

/// Relevance tier for a symbol in completion: nearer scopes and more
/// recently declared names sort first.
fn symbol_relevance(symbol: &Symbol, offset: usize) -> u8 {
    let distance = symbol.name_span.start.abs_diff(offset);
    let proximity = match symbol.kind {
        SymbolKind::Parameter => 0,
        SymbolKind::Variable => 1,
        SymbolKind::Field => 2,
        SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Method => 3,
        SymbolKind::Class | SymbolKind::Enum | SymbolKind::Trait | SymbolKind::TypeAlias => 4,
        _ => 5,
    };
    // Only the first tier splits by distance, and only coarsely, so the
    // list stays stable while typing.
    proximity + u8::from(distance > 4000)
}

/// Markdown documentation for a completion item.
fn documentation_of(docs: &str) -> Option<lsp_types::Documentation> {
    let docs = docs.trim();
    if docs.is_empty() {
        return None;
    }
    Some(lsp_types::Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: docs.to_string(),
    }))
}

/// Whether the cursor sits in expression position, where declaration
/// keywords would be noise. Looks at the text before the partial word on
/// the current line.
fn in_expression_context(prefix: &str, partial: &str) -> bool {
    let before = prefix.strip_suffix(partial).unwrap_or(prefix).trim_end();
    let Some(last) = before.chars().last() else {
        // Start of an empty line: declarations are plausible.
        return false;
    };
    matches!(
        last,
        '(' | '['
            | ','
            | '='
            | '+'
            | '-'
            | '*'
            | '/'
            | '%'
            | '<'
            | '>'
            | '!'
            | '&'
            | '|'
            | '^'
            | '?'
            | ':'
            | '~'
    )
}

fn is_use_context(prefix: &str) -> bool {
    let trimmed = prefix.trim_start();
    trimmed == "use"
        || trimmed.starts_with("use ")
        || trimmed.starts_with("use\"")
        || trimmed.starts_with("use \"")
        || trimmed == "from"
        || trimmed.starts_with("from ")
}

fn trailing_identifier(prefix: &str) -> &str {
    let end = prefix
        .len()
        .saturating_sub(prefix.len() - prefix.trim_end().len());
    let trimmed = &prefix[..end];
    let start = trimmed
        .rfind(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .map_or(0, |index| index + 1);
    &trimmed[start..]
}

fn snippet_completions(partial: &str) -> Vec<CompletionItem> {
    const SNIPPETS: &[(&str, &str, &str)] = &[
        (
            "fun",
            "Function declaration",
            "fun ${1:name}(${2:params}) {\n\t$0\n}",
        ),
        ("test", "Test block", "test ${1:name} {\n\t$0\n}"),
        ("class", "Class declaration", "class ${1:Name} {\n\t$0\n}"),
        ("if", "If statement", "if (${1:cond}) {\n\t$0\n}"),
        (
            "for",
            "For loop",
            "for (${1:init}; ${2:cond}; ${3:update}) {\n\t$0\n}",
        ),
        (
            "match",
            "Match statement",
            "match (${1:value}) {\n\tcase ${2:pattern} => $0\n}",
        ),
    ];
    SNIPPETS
        .iter()
        .filter(|(label, _, _)| partial.is_empty() || label.starts_with(partial))
        .map(|(label, detail, snippet)| CompletionItem {
            label: (*label).to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some((*detail).to_string()),
            insert_text: Some((*snippet).to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..CompletionItem::default()
        })
        .collect()
}

fn is_valid_name(name: &str) -> bool {
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
    {
        return false;
    }
    let tokens = ntsc_lexer::tokenize(name);
    matches!(tokens.first(), Some(token) if matches!(&token.kind, ntsc_ast::token::TokenKind::Identifier(identifier) if identifier == name))
        && tokens
            .iter()
            .skip(1)
            .all(|token| token.kind == ntsc_ast::token::TokenKind::Eof)
}

fn ranges_overlap(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
}

/// Sort ranges by start position and drop duplicates.
fn sort_ranges(ranges: &mut Vec<Range>) {
    ranges.sort_by(|left, right| {
        position_key(left.start)
            .cmp(&position_key(right.start))
            .then(position_key(left.end).cmp(&position_key(right.end)))
    });
    ranges.dedup();
}

fn position_key(position: lsp_types::Position) -> (u32, u32) {
    (position.line, position.character)
}

/// Zero-based argument index at `offset`: commas at paren depth zero before
/// the cursor, skipping over strings and nested brackets.
fn active_parameter(text: &str, call: &crate::analysis::CallSite, offset: usize) -> u32 {
    let bytes = text.as_bytes();
    let mut depth = 0u32;
    let mut commas = 0u32;
    let mut index = call.paren_span.start.saturating_add(1);
    let end = offset.min(text.len()).min(call.full_span.end.max(index));
    let mut in_string: Option<u8> = None;
    while index < end.min(bytes.len()) {
        let byte = bytes[index];
        if let Some(quote) = in_string {
            if byte == b'\\' {
                index += 1;
            } else if byte == quote {
                in_string = None;
            }
        } else {
            match byte {
                b'"' | b'\'' => in_string = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth > 0 => depth -= 1,
                b',' if depth == 0 => commas += 1,
                _ => {}
            }
        }
        index += 1;
    }
    commas
}

/// Semantic-token legend; indices below must match [`token_legend`].
mod token_index {
    pub const NAMESPACE: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const CLASS: u32 = 2;
    pub const ENUM: u32 = 3;
    pub const INTERFACE: u32 = 4;
    #[allow(dead_code)]
    pub const STRUCT: u32 = 5;
    #[allow(dead_code)]
    pub const TYPE_PARAMETER: u32 = 6;
    pub const PARAMETER: u32 = 7;
    pub const VARIABLE: u32 = 8;
    pub const PROPERTY: u32 = 9;
    pub const ENUM_MEMBER: u32 = 10;
    #[allow(dead_code)]
    pub const EVENT: u32 = 11;
    pub const FUNCTION: u32 = 12;
    pub const METHOD: u32 = 13;
    #[allow(dead_code)]
    pub const MACRO: u32 = 14;
    pub const KEYWORD: u32 = 15;
    #[allow(dead_code)]
    pub const MODIFIER: u32 = 16;
    #[allow(dead_code)]
    pub const COMMENT: u32 = 17;
    pub const STRING: u32 = 18;
    pub const NUMBER: u32 = 19;
    #[allow(dead_code)]
    pub const REGEXP: u32 = 20;
    pub const OPERATOR: u32 = 21;
    #[allow(dead_code)]
    pub const DECORATOR: u32 = 22;
}

mod token_modifier {
    pub const DECLARATION: u32 = 0b001;
    pub const DEFINITION: u32 = 0b010;
    #[allow(dead_code)]
    pub const READONLY: u32 = 0b100;
}

fn token_legend() -> SemanticTokensLegend {
    use lsp_types::SemanticTokenType as T;
    SemanticTokensLegend {
        token_types: vec![
            T::NAMESPACE,
            T::TYPE,
            T::CLASS,
            T::ENUM,
            T::INTERFACE,
            T::STRUCT,
            T::TYPE_PARAMETER,
            T::PARAMETER,
            T::VARIABLE,
            T::PROPERTY,
            T::ENUM_MEMBER,
            T::EVENT,
            T::FUNCTION,
            T::METHOD,
            T::MACRO,
            T::KEYWORD,
            T::MODIFIER,
            T::COMMENT,
            T::STRING,
            T::NUMBER,
            T::REGEXP,
            T::OPERATOR,
            T::DECORATOR,
        ],
        token_modifiers: vec![
            lsp_types::SemanticTokenModifier::DECLARATION,
            lsp_types::SemanticTokenModifier::DEFINITION,
            lsp_types::SemanticTokenModifier::READONLY,
        ],
    }
}

fn is_realistic_span(span: ntsc_ast::span::Span) -> bool {
    span.start != 0 || span.end != 0
}

fn semantic_token_data(document: &Document) -> Vec<lsp_types::SemanticToken> {
    use ntsc_ast::token::TokenKind as K;
    let mut entries: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    for (token_idx, token) in document.analysis.tokens.iter().enumerate() {
        let span = token.span;
        if !is_realistic_span(span) || span.end <= span.start {
            continue;
        }
        let Some(slice) = document
            .text
            .get(span.start..span.end.min(document.text.len()))
        else {
            continue;
        };
        if slice.contains('\n') {
            continue;
        }
        let position = document.index.position(&document.text, span.start);
        let length = slice.chars().map(|ch| ch.len_utf16() as u32).sum::<u32>();
        if length == 0 {
            continue;
        }
        let (token_type, modifiers) = match &token.kind {
            K::NumberLiteral(_) => (token_index::NUMBER, 0),
            K::StringLiteral(_) | K::StringSegment(_) => (token_index::STRING, 0),
            K::Identifier(_) => identifier_token_class(document, token_idx),
            _ if is_keyword(&token.kind) => (token_index::KEYWORD, 0),
            K::TypeInt
            | K::TypeFloat
            | K::TypeString
            | K::TypeBool
            | K::TypeArray
            | K::TypeObject
            | K::TypeOption
            | K::TypeResult
            | K::TypeAny
            | K::TypePointer
            | K::TypeSlice => (token_index::TYPE, 0),
            K::Newline | K::Eof => continue,
            _ => (token_index::OPERATOR, 0),
        };
        entries.push((
            position.line,
            position.character,
            length,
            token_type,
            modifiers,
        ));
    }
    entries.sort();
    let mut data = Vec::with_capacity(entries.len());
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for (line, col, length, token_type, modifiers) in entries {
        let delta_line = line - prev_line;
        let delta_col = if delta_line == 0 { col - prev_col } else { col };
        data.push(lsp_types::SemanticToken {
            delta_line,
            delta_start: delta_col,
            length,
            token_type,
            token_modifiers_bitset: modifiers,
        });
        prev_line = line;
        prev_col = col;
    }
    data
}

fn identifier_token_class(document: &Document, token_idx: usize) -> (u32, u32) {
    let token = &document.analysis.tokens[token_idx];
    let span = token.span;
    // A property follows `.` / `?.` (skipping nothing: the lexer emits no
    // whitespace tokens, but newlines are possible before the dot).
    let is_property = (0..token_idx)
        .rev()
        .take_while(|idx| {
            matches!(
                document.analysis.tokens[*idx].kind,
                ntsc_ast::token::TokenKind::Newline
            )
        })
        .count()
        == 0
        && token_idx > 0
        && matches!(
            document.analysis.tokens[token_idx - 1].kind,
            ntsc_ast::token::TokenKind::Dot | ntsc_ast::token::TokenKind::QuestionDot
        );
    let mut reference_match: Option<&crate::analysis::Reference> = None;
    for reference in &document.analysis.references {
        if reference.span == span {
            reference_match = Some(reference);
            break;
        }
    }
    if let Some(reference) = reference_match {
        if let Some(target) = reference
            .target
            .and_then(|idx| document.analysis.symbols.get(idx))
        {
            let token_type = match target.kind {
                SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Test => {
                    token_index::FUNCTION
                }
                SymbolKind::Method => token_index::METHOD,
                SymbolKind::Class => token_index::CLASS,
                SymbolKind::Enum => token_index::ENUM,
                SymbolKind::EnumMember => token_index::ENUM_MEMBER,
                SymbolKind::Trait => token_index::INTERFACE,
                SymbolKind::TypeAlias => token_index::TYPE,
                SymbolKind::Parameter => token_index::PARAMETER,
                SymbolKind::Field => token_index::PROPERTY,
                SymbolKind::Variable => token_index::VARIABLE,
                SymbolKind::Module => token_index::NAMESPACE,
            };
            let modifiers = if reference.is_definition {
                token_modifier::DECLARATION | token_modifier::DEFINITION
            } else {
                0
            };
            return (token_type, modifiers);
        }
        if reference.is_member || is_property {
            return (token_index::PROPERTY, 0);
        }
        // Unresolved names still get a sane default; also cover stdlib
        // modules and builtins used without an import.
        if stdlib::module(&reference.name).is_some() {
            return (token_index::NAMESPACE, 0);
        }
        return (token_index::VARIABLE, 0);
    }
    if is_property {
        return (token_index::PROPERTY, 0);
    }
    (token_index::VARIABLE, 0)
}

fn is_keyword(kind: &ntsc_ast::token::TokenKind) -> bool {
    use ntsc_ast::token::TokenKind as K;
    matches!(
        kind,
        K::And
            | K::Class
            | K::Else
            | K::Elif
            | K::False
            | K::Fun
            | K::For
            | K::If
            | K::Nil
            | K::Or
            | K::Say
            | K::Return
            | K::Static
            | K::Super
            | K::This
            | K::True
            | K::Var
            | K::While
            | K::Do
            | K::Break
            | K::Continue
            | K::Match
            | K::Case
            | K::Default
            | K::Try
            | K::Catch
            | K::Finally
            | K::Throw
            | K::Retry
            | K::Unsafe
            | K::Quiet
            | K::Enum
            | K::Type
            | K::Trait
            | K::Impl
            | K::In
            | K::Use
            | K::From
            | K::As
            | K::Test
            | K::Async
            | K::Await
            | K::View
            | K::Mut
            | K::Shared
            | K::Copy
            | K::Own
            | K::Go
            | K::Chan
            | K::Close
    )
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
