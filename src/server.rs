//! LSP lifecycle and transport. Language analysis remains editor independent.
use crate::{
    analysis::{Cancellation, Engine, ReferenceSearch},
    scheduler::{Inbox, Job, Work},
    source::{Position, Range, Source},
    workspace::{self, Scan, ScanEvent},
};
use anyhow::{Context, Result};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocument {
    uri: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Query {
    text_document: TextDocument,
    #[serde(default)]
    position: Position,
    #[serde(default)]
    context: Value,
    #[serde(default)]
    new_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    range: Option<Range>,
    text: String,
}

pub fn run_stdio() -> Result<()> {
    let (connection, threads) = Connection::stdio();
    serve(&connection)?;
    drop(connection);
    threads.join()?;
    Ok(())
}
pub fn serve(connection: &Connection) -> Result<()> {
    let (id, mut params) = connection.initialize_start()?;
    canonicalize_uris(&mut params);
    connection.initialize_finish(id, json!({
        "capabilities": {
            "positionEncoding":"utf-16", "textDocumentSync":{"openClose":true,"change":2,"save":{}},
            "definitionProvider":true, "referencesProvider":true, "hoverProvider":true,
            "documentHighlightProvider":true, "documentSymbolProvider":true,"workspaceSymbolProvider":true,
            "completionProvider":{"triggerCharacters":[".","!","(",","]},
            "signatureHelpProvider":{"triggerCharacters":["(",",","="]},
            "renameProvider":true,"codeLensProvider":{"resolveProvider":false},
            "workspace":{"workspaceFolders":{"supported":true,"changeNotifications":true}}
        },
        "serverInfo":{"name":"gorak-lsp-rs","version":env!("CARGO_PKG_VERSION")}
    }))?;
    let mut server = Server::new(roots(&params), &params["initializationOptions"]);
    server.snippets = params
        .pointer("/capabilities/textDocument/completion/completionItem/snippetSupport")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let inbox = Inbox::new(connection);
    let mut shutdown = false;
    loop {
        if let Some(work) = inbox.next(server.complete) {
            match work {
                Work::Request(mut job, cancellation) => {
                    let response = if shutdown {
                        Some(Response::new_err(
                            job.request.id.clone(),
                            ErrorCode::InvalidRequest as i32,
                            "Server is shutting down".into(),
                        ))
                    } else if job.request.method == "shutdown" {
                        shutdown = true;
                        Some(Response::new_ok(job.request.id.clone(), Value::Null))
                    } else if job.request.method == "gorak/rebuildIndex" {
                        if job.waiting_for_index {
                            Some(Response::new_ok(
                                job.request.id.clone(),
                                json!({"rebuilt":true}),
                            ))
                        } else {
                            server.restart(true)?;
                            None
                        }
                    } else {
                        server.engine.cancellation = cancellation;
                        Some(server.request(connection, &mut job)?)
                    };
                    inbox.finish(job, response);
                    server.engine.cancellation = Cancellation::default();
                }
                Work::Notification(notification) => {
                    if notification.method == "exit" {
                        break;
                    }
                    if !shutdown {
                        server.notification(connection, notification)?;
                    }
                }
            }
            continue;
        }
        if inbox.disconnected() {
            break;
        }
        if !shutdown {
            server.publish_due(connection)?;
        }
        let diagnostic_timer = server
            .diagnostics_due
            .filter(|_| !shutdown)
            .map_or_else(crossbeam_channel::never, |due| {
                crossbeam_channel::after(due.saturating_duration_since(Instant::now()))
            });
        let events = if shutdown {
            crossbeam_channel::never()
        } else {
            server.scan.events.clone()
        };
        crossbeam_channel::select_biased! {
            recv(inbox.wake) -> _ => {},
            recv(diagnostic_timer) -> _ => {},
            recv(events) -> event => match event {
                Ok(event) => server.scan_event(connection, event)?,
                Err(_) => server.scan.events = crossbeam_channel::never(),
            }
        }
    }
    Ok(())
}
fn roots(params: &Value) -> Vec<PathBuf> {
    let uris: Vec<&str> = params
        .get("workspaceFolders")
        .and_then(Value::as_array)
        .map(|folders| {
            folders
                .iter()
                .filter_map(|f| f.get("uri").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_else(|| {
            params
                .get("rootUri")
                .and_then(Value::as_str)
                .into_iter()
                .collect()
        });
    uris.into_iter()
        .filter_map(file_path)
        .map(|path| {
            path.ancestors()
                .find(|candidate| candidate.join("gorak.json").is_file())
                .map_or_else(|| path.clone(), |root| root.to_path_buf())
        })
        .collect()
}
fn file_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}
struct Server {
    engine: Engine,
    cache: Option<crate::cache::Cache>,
    scan: Scan,
    overlays: HashSet<String>,
    changed: HashSet<String>,
    complete: bool,
    indexed: usize,
    failures: usize,
    cache_hits: usize,
    roots: Vec<PathBuf>,
    snippets: bool,
    details: crate::details::Details,
    metadata_overlays: HashMap<String, (Source, i32)>,
    history: VecDeque<serde_json::Value>,
    last_progress: Instant,
    diagnostics_due: Option<Instant>,
    diagnostics_pending: HashSet<String>,
}
impl Server {
    fn new(roots: Vec<PathBuf>, options: &Value) -> Self {
        let cache = options["cacheDirectory"]
            .as_str()
            .map(|path| crate::cache::Cache::new(PathBuf::from(path)))
            .or_else(crate::cache::Cache::user_default);
        let budget = options["memoryBudgetMB"]
            .as_u64()
            .unwrap_or(32)
            .clamp(8, 512) as usize
            * 1024
            * 1024;
        Self {
            engine: Engine::default(),
            scan: Scan::start(roots.clone(), cache.clone()),
            roots,
            snippets: false,
            details: crate::details::Details::new(budget, cache.clone()),
            cache,
            metadata_overlays: HashMap::new(),
            history: VecDeque::new(),
            last_progress: Instant::now(),
            diagnostics_due: None,
            diagnostics_pending: HashSet::new(),
            overlays: HashSet::new(),
            changed: HashSet::new(),
            complete: false,
            indexed: 0,
            failures: 0,
            cache_hits: 0,
        }
    }
    fn restart(&mut self, force: bool) -> Result<()> {
        let overlays = self
            .overlays
            .iter()
            .filter_map(|uri| {
                self.engine.id(uri).map(|id| {
                    (
                        self.engine.physical(id).clone(),
                        self.engine.entry(id).version,
                    )
                })
            })
            .collect::<Vec<_>>();
        self.engine = Engine::default();
        for (document, version) in overlays {
            self.engine
                .insert(document, version)
                .map_err(anyhow::Error::msg)?;
        }
        for (uri, (source, _)) in &self.metadata_overlays {
            self.engine
                .graph
                .set(crate::analysis::index::application(uri), source.text());
        }
        self.scan = Scan::start(
            self.roots.clone(),
            if force { None } else { self.cache.clone() },
        );
        self.complete = false;
        self.indexed = 0;
        self.cache_hits = 0;
        self.failures = 0;
        self.changed = self.overlays.clone();
        Ok(())
    }
    fn request(&mut self, connection: &Connection, job: &mut Job) -> Result<Response> {
        let mut request = job.request.clone();
        canonicalize_uris(&mut request.params);
        let start = Instant::now();
        let loaded = self.prepare_request(&request);
        let result = loaded.and_then(|()| {
            if self.engine.interrupted() {
                return Err((ErrorCode::RequestCanceled, "Request interrupted".into()));
            }
            if request.method == "textDocument/references" {
                self.reference_query(request.params, &mut job.references)
            } else {
                self.query(&request.method, request.params)
            }
        });
        let outcome = if self.engine.interrupted() {
            "interrupted"
        } else if result.is_ok() {
            "ok"
        } else {
            "error"
        };
        self.history.push_back(json!({"method":request.method,"durationMs":start.elapsed().as_secs_f64()*1000.0,"outcome":outcome}));
        if self.history.len() > 50 {
            self.history.pop_front();
        }
        let response = match result {
            Ok(value) => Response::new_ok(request.id, value),
            Err((code, message)) => Response::new_err(request.id, code as i32, message),
        };
        self.details.trim(&mut self.engine, &self.overlays);
        if start.elapsed().as_millis() > 100 {
            notify(
                connection,
                "window/logMessage",
                json!({"type":3,"message":format!("{} took {} ms",request.method,start.elapsed().as_millis())}),
            )?;
        }
        Ok(response)
    }
    fn prepare_request(
        &mut self,
        request: &Request,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        let Some(uri) = request
            .params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        if matches!(
            request.method.as_str(),
            "textDocument/documentSymbol" | "textDocument/codeLens"
        ) {
            return Ok(());
        }
        self.details.load(&mut self.engine, uri).map_err(|e| {
            (
                ErrorCode::InternalError,
                format!("Cannot load document details: {e}"),
            )
        })?;
        self.engine.prepare();
        if request.method == "textDocument/rename" {
            let position: Position = serde_json::from_value(request.params["position"].clone())
                .map_err(|e| (ErrorCode::InvalidParams, e.to_string()))?;
            let target = self.engine.target(uri, position);
            if target.len() == 1 {
                let symbol = self.engine.symbol(target[0]);
                let owner_name = symbol.owner.map(|owner| {
                    self.engine
                        .document(target[0].document)
                        .symbol(owner)
                        .spelling
                        .clone()
                });
                let name = symbol.spelling.clone();
                if request.method == "textDocument/rename"
                    && let Some(owner_name) = owner_name
                {
                    self.details
                        .load_named(&mut self.engine, &owner_name)
                        .map_err(|e| {
                            (
                                ErrorCode::InternalError,
                                format!("Cannot load callable references: {e}"),
                            )
                        })?;
                }
                self.details
                    .load_named(&mut self.engine, &name)
                    .map_err(|e| {
                        (
                            ErrorCode::InternalError,
                            format!("Cannot load reference candidates: {e}"),
                        )
                    })?;
                self.engine.prepare();
            }
        }
        Ok(())
    }
    fn reference_query(
        &mut self,
        params: Value,
        progress: &mut Option<ReferenceSearch>,
    ) -> std::result::Result<Value, (ErrorCode, String)> {
        let query: Query = serde_json::from_value(params)
            .map_err(|e| (ErrorCode::InvalidParams, e.to_string()))?;
        if progress.is_none() {
            let targets = self.engine.target(&query.text_document.uri, query.position);
            if targets.len() != 1 {
                return Ok(json!([]));
            }
            let target = self.engine.canonical(targets[0]);
            *progress = Some(ReferenceSearch {
                target,
                name: self.engine.symbol(target).spelling.clone(),
                next_document: 0,
                locations: Vec::new(),
            });
        }
        let search = progress.as_mut().unwrap();
        let include_declaration = query.context["includeDeclaration"]
            .as_bool()
            .unwrap_or(false);
        // Keep the query document resident, but never pin all matching files.
        let mut pinned = self.overlays.clone();
        pinned.insert(query.text_document.uri);
        while search.next_document < self.engine.entries.len() {
            let i = search.next_document;
            if self.engine.interrupted() {
                break;
            }
            let id = crate::analysis::index::DocumentId(i);
            if self.engine.document(id).names.find(&search.name).is_none() {
                search.next_document += 1;
                continue;
            }
            let uri = self.engine.document(id).source.uri.clone();
            self.details.load(&mut self.engine, &uri).map_err(|e| {
                (
                    ErrorCode::InternalError,
                    format!("Cannot load reference candidates: {e}"),
                )
            })?;
            self.engine.prepare();
            let found = self
                .engine
                .references_in(search.target, id, include_declaration);
            if !self.engine.interrupted() {
                search.locations.extend(found);
                search.next_document += 1;
            }
            self.details.trim(&mut self.engine, &pinned);
        }
        if self.engine.interrupted() {
            return Err((ErrorCode::RequestCanceled, "Request interrupted".into()));
        }
        search.locations.sort_by(|a, b| {
            (&a.uri, a.range.start.line, a.range.start.character).cmp(&(
                &b.uri,
                b.range.start.line,
                b.range.start.character,
            ))
        });
        search.locations.dedup();
        Ok(json!(search.locations))
    }
    fn query(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, (ErrorCode, String)> {
        if method == "gorak/indexStatus" {
            return Ok(json!({
                "indexing": !self.complete,
                "files": self.indexed,
                "failures": self.failures,
                "engine": "rust",
                "serverVersion": env!("CARGO_PKG_VERSION"),
                "cacheHits": self.cache_hits,
                "restoredFiles": self.cache_hits,
                "parsedFiles": self.indexed - self.cache_hits,
                "applications": self.engine.graph.applications.len(),
                "phase": if self.complete { "ready" } else { "scanning" },
                "failed": self.failures > 0,
                "detailCacheBytes": self.details.bytes(&self.engine),
                "requests": self.history,
                "rssBytes": rss_bytes()
            }));
        }
        if method == "gorak/builtinSource" {
            return Ok(json!(
                self.engine
                    .builtin_source(params["uri"].as_str().unwrap_or_default())
            ));
        }
        if method == "gorak/resolve" {
            let query: Query = serde_json::from_value(params)
                .map_err(|e| (ErrorCode::InvalidParams, e.to_string()))?;
            return Ok(self
                .engine
                .explain(&query.text_document.uri, query.position));
        }
        if method == "workspace/symbol" {
            return Ok(self.engine.workspace_symbols(
                params
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ));
        }
        if !matches!(
            method,
            "textDocument/definition"
                | "textDocument/references"
                | "textDocument/documentHighlight"
                | "textDocument/hover"
                | "textDocument/completion"
                | "textDocument/signatureHelp"
                | "textDocument/documentSymbol"
                | "textDocument/rename"
                | "textDocument/codeLens"
        ) {
            return Err((
                ErrorCode::MethodNotFound,
                format!("Unsupported request: {method}"),
            ));
        }
        let query: Query = serde_json::from_value(params)
            .map_err(|e| (ErrorCode::InvalidParams, e.to_string()))?;
        let uri = &query.text_document.uri;
        let position = query.position;
        Ok(match method {
            "textDocument/definition" => json!(self.engine.definitions(uri, position)),
            "textDocument/references" => json!(
                self.engine.references(
                    uri,
                    position,
                    query
                        .context
                        .get("includeDeclaration")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    false
                )
            ),
            "textDocument/documentHighlight" => json!(
                self.engine
                    .references(uri, position, true, true)
                    .iter()
                    .filter(|l| l.uri.as_ref() == uri)
                    .map(|l| json!({"range":l.range,"kind":1}))
                    .collect::<Vec<_>>()
            ),
            "textDocument/hover" => self.engine.hover(uri, position),
            "textDocument/completion" => {
                json!({"isIncomplete":!self.complete,"items":self.engine.completions_with_snippets(uri,position,self.snippets)})
            }
            "textDocument/signatureHelp" => self.engine.signature_help(uri, position),
            "textDocument/documentSymbol" => self.engine.document_symbols(uri),
            "textDocument/codeLens" => self.engine.code_lenses(uri),
            "textDocument/rename" => {
                if !self.complete || self.failures > 0 {
                    return Err((
                        ErrorCode::RequestFailed,
                        "Rename requires a complete workspace index without unreadable files."
                            .into(),
                    ));
                }
                self.engine
                    .rename(uri, position, &query.new_name)
                    .map_err(|e| (ErrorCode::RequestFailed, e))?
            }
            _ => unreachable!("method was validated before parsing parameters"),
        })
    }
    fn notification(
        &mut self,
        connection: &Connection,
        mut notification: Notification,
    ) -> Result<()> {
        canonicalize_uris(&mut notification.params);
        if let Err(error) = self.apply_notification(connection, notification) {
            notify(
                connection,
                "window/logMessage",
                json!({"type":1,"message":format!("Document update failed: {error:#}")}),
            )?;
        }
        Ok(())
    }
    fn apply_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<()> {
        let params = notification.params;
        match notification.method.as_str() {
            "textDocument/didOpen" => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().context("Missing document URI")?;
                let text = doc["text"].as_str().context("Missing document text")?;
                let version = version(doc)?;
                if uri.ends_with("/app.json") {
                    self.metadata_overlays.insert(
                        uri.into(),
                        (Source::new(uri, text).map_err(anyhow::Error::msg)?, version),
                    );
                    self.engine
                        .graph
                        .set(crate::analysis::index::application(uri), text);
                    self.engine.invalidate_expansions();
                    return Ok(());
                }

                self.engine
                    .update(uri, text, version)
                    .map_err(anyhow::Error::msg)?;
                self.overlays.insert(uri.into());
                self.changed.insert(uri.into());
                self.queue_diagnostics(uri);
            }
            "textDocument/didChange" => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().context("Missing document URI")?;

                if let Some((source, prior)) = self.metadata_overlays.get(uri) {
                    let next = version(doc)?;
                    if next <= *prior {
                        anyhow::bail!("Rejected stale metadata version");
                    }
                    let source = apply_changes(source.clone(), &params["contentChanges"])?;
                    self.engine
                        .graph
                        .set(crate::analysis::index::application(uri), source.text());
                    self.engine.invalidate_expansions();
                    self.metadata_overlays.insert(uri.into(), (source, next));
                    return Ok(());
                }
                let id = self
                    .engine
                    .id(uri)
                    .context("Change received before document open")?;
                let version = version(doc)?;
                if version <= self.engine.entry(id).version {
                    anyhow::bail!("Rejected stale document version");
                }
                let source = apply_changes(
                    self.engine.physical(id).source.clone(),
                    &params["contentChanges"],
                )?;
                self.engine
                    .update(uri, source.text(), version)
                    .map_err(anyhow::Error::msg)?;
                self.queue_diagnostics(uri);
            }
            "textDocument/didClose" => {
                let uri = params["textDocument"]["uri"]
                    .as_str()
                    .context("Missing document URI")?;
                self.metadata_overlays.remove(uri);
                self.diagnostics_pending.remove(uri);
                self.overlays.remove(uri);
                self.refresh(uri)?;
                notify(
                    connection,
                    "textDocument/publishDiagnostics",
                    json!({"uri":uri,"diagnostics":[]}),
                )?;
            }
            "workspace/didChangeWorkspaceFolders" => {
                if let Some(removed) = params.pointer("/event/removed").and_then(Value::as_array) {
                    let removed = roots(&json!({"workspaceFolders":removed}));
                    self.roots.retain(|root| !removed.contains(root));
                }
                if let Some(added) = params.pointer("/event/added").and_then(Value::as_array) {
                    for root in roots(&json!({"workspaceFolders":added})) {
                        if !self.roots.contains(&root) {
                            self.roots.push(root);
                        }
                    }
                }
                self.restart(false)?;
            }
            "workspace/didChangeWatchedFiles" => {
                if let Some(changes) = params["changes"].as_array() {
                    for change in changes {
                        if let Some(uri) = change["uri"].as_str() {
                            self.changed.insert(uri.into());
                            self.refresh(uri)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn refresh(&mut self, uri: &str) -> Result<()> {
        if self.overlays.contains(uri) || self.metadata_overlays.contains_key(uri) {
            return Ok(());
        }
        let Some(path) = file_path(uri) else {
            return Ok(());
        };
        if !path.try_exists()? {
            if path.file_name().is_some_and(|name| name == "app.json") {
                self.engine
                    .graph
                    .set(crate::analysis::index::application(uri), "{}");
                self.engine.invalidate_expansions();
            } else {
                self.engine.remove(uri);
            }
            return Ok(());
        }
        match workspace::load(&path) {
            Some(ScanEvent::Document { document, .. }) => {
                self.engine
                    .insert(*document, 0)
                    .map_err(anyhow::Error::msg)?;
            }
            Some(ScanEvent::Application { directory, text }) => {
                self.engine.graph.set(directory, &text);
                self.engine.invalidate_expansions();
            }
            Some(ScanEvent::Failed(message)) => {
                self.failures += 1;
                anyhow::bail!(message);
            }
            _ => {}
        }
        Ok(())
    }
    fn queue_diagnostics(&mut self, uri: &str) {
        self.diagnostics_pending.insert(uri.into());
        self.diagnostics_due = Some(Instant::now() + Duration::from_millis(150));
    }
    fn publish_due(&mut self, connection: &Connection) -> Result<()> {
        if self.diagnostics_due.is_none_or(|due| Instant::now() < due) {
            return Ok(());
        }
        self.diagnostics_due = None;
        for uri in std::mem::take(&mut self.diagnostics_pending) {
            if self.overlays.contains(&uri) {
                self.publish(connection, &uri)?;
            }
        }
        Ok(())
    }
    fn publish(&self, connection: &Connection, uri: &str) -> Result<()> {
        let version = self.engine.id(uri).map(|id| self.engine.entry(id).version);
        notify(
            connection,
            "textDocument/publishDiagnostics",
            json!({"uri":uri,"version":version,"diagnostics":self.engine.diagnostics(uri)}),
        )
    }
    fn scan_event(&mut self, connection: &Connection, event: ScanEvent) -> Result<()> {
        match event {
            ScanEvent::Application { directory, text } => {
                let uri =
                    url::Url::from_file_path(std::path::Path::new(&directory).join("app.json"))
                        .ok()
                        .map(|u| crate::source::canonical_uri(u.as_str()));
                if uri.as_ref().is_some_and(|uri| {
                    self.metadata_overlays.contains_key(uri) || self.changed.contains(uri)
                }) {
                    return Ok(());
                }
                self.engine.graph.set(directory, &text);
                self.engine.invalidate_expansions();
            }
            ScanEvent::Document { document, cached } => {
                self.cache_hits += usize::from(cached);
                let document_uri = document.source.uri.clone();
                if !self.changed.contains(document.source.uri.as_ref()) {
                    self.engine
                        .insert(*document, 0)
                        .map_err(anyhow::Error::msg)?;
                }
                self.details.observe(&document_uri);
                self.indexed += 1;
                self.details.trim(&mut self.engine, &self.overlays);
            }
            ScanEvent::Failed(message) => {
                self.failures += 1;
                notify(
                    connection,
                    "window/logMessage",
                    json!({"type":2,"message":message}),
                )?;
            }
            ScanEvent::Complete => {
                self.engine.prepare();
                for entry in &self.engine.entries {
                    if entry.document.source.is_resident() {
                        self.details.observe(&entry.document.source.uri);
                    }
                }
                self.complete = true;
                self.details.trim(&mut self.engine, &self.overlays);
                for uri in self.overlays.clone() {
                    self.queue_diagnostics(&uri);
                }
                notify(
                    connection,
                    "window/logMessage",
                    json!({"type":3,"message":format!("Rust workspace index ready: {} files, {} read failures",self.indexed,self.failures)}),
                )?;
            }
        }
        if self.complete || self.last_progress.elapsed().as_millis() >= 250 {
            notify(
                connection,
                "gorak/indexStatus",
                self.query("gorak/indexStatus", Value::Null)
                    .expect("status is infallible"),
            )?;
            self.last_progress = Instant::now();
        }
        Ok(())
    }
}
fn version(doc: &Value) -> Result<i32> {
    doc["version"]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .context("Missing or invalid document version")
}
fn notify(connection: &Connection, method: &str, params: Value) -> Result<()> {
    connection
        .sender
        .send(Message::Notification(Notification::new(
            method.into(),
            params,
        )))?;
    Ok(())
}

fn apply_changes(mut source: Source, changes: &Value) -> Result<Source> {
    let changes: Vec<Change> = serde_json::from_value(changes.clone())?;
    for change in changes {
        source = match change.range {
            Some(range) => source.replace(range, &change.text),
            None => Source::new(source.uri.clone(), change.text),
        }
        .map_err(anyhow::Error::msg)?;
    }
    Ok(source)
}
fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|k| k * 1024)
}

fn canonicalize_uris(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "uri" | "rootUri" | "oldUri" | "newUri") {
                    if let Some(uri) = value.as_str() {
                        *value = json!(crate::source::canonical_uri(uri));
                    }
                } else {
                    canonicalize_uris(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                canonicalize_uris(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_references_match_resident_results_without_retaining_candidates() {
        let directory = tempfile::tempdir().unwrap();
        let mut server = Server::new(Vec::new(), &json!({}));
        let class_uri = url::Url::from_file_path(directory.path().join("record.w4gl"))
            .unwrap()
            .to_string();
        let class = "[classsource]\n===\n";
        std::fs::write(file_path(&class_uri).unwrap(), class).unwrap();
        server.engine.update(&class_uri, class, 0).unwrap();
        let mut query = Value::Null;
        for i in 0..8 {
            let uri = url::Url::from_file_path(directory.path().join(format!("p{i}.w4gl")))
                .unwrap()
                .to_string();
            let text = format!(
                "[proc4glsource]\n===\nPROCEDURE p{i}() = DECLARE item = record; {{ {} }}",
                "MESSAGE item;\n".repeat(2000)
            );
            std::fs::write(file_path(&uri).unwrap(), &text).unwrap();
            server.engine.update(&uri, &text, 0).unwrap();
            if i == 0 {
                query = json!({"textDocument":{"uri":uri},"position":Source::new("file:///test", &*text).unwrap().position(text.find("record;").unwrap() as u32),"context":{"includeDeclaration":true}});
            }
        }
        let origin = query["textDocument"]["uri"].as_str().unwrap();
        let position = serde_json::from_value(query["position"].clone()).unwrap();
        let expected = json!(server.engine.references(origin, position, true, false));
        assert_eq!(expected.as_array().unwrap().len(), 9);
        server.details = crate::details::Details::new(1, None);
        for entry in &server.engine.entries {
            server.details.observe(&entry.document.source.uri);
        }
        let pinned = HashSet::from([origin.to_owned()]);
        server.details.trim(&mut server.engine, &pinned);
        let mut progress = None;
        let result = server
            .reference_query(query.clone(), &mut progress)
            .unwrap();
        assert_eq!(result, expected);
        assert_eq!(
            server
                .engine
                .entries
                .iter()
                .filter(|e| e.document.source.is_resident())
                .count(),
            1
        );
        // A retry after the final checkpoint must preserve the complete result.
        assert_eq!(
            server.reference_query(query, &mut progress).unwrap(),
            expected
        );
    }
    #[test]
    fn diagnostic_bursts_publish_only_the_latest_open_document_version() {
        let (client, connection) = Connection::memory();
        let mut server = Server::new(Vec::new(), &json!({}));
        let uri = "file:///synthetic/app/main.w4gl";
        for version in 1..=3 {
            server
                .engine
                .update(uri, "[proc4glsource]\n===\nPROCEDURE main() = { }", version)
                .unwrap();
            server.overlays.insert(uri.into());
            server.queue_diagnostics(uri);
            server.publish_due(&connection).unwrap();
        }
        assert!(client.receiver.try_recv().is_err());
        server.diagnostics_due = Some(Instant::now());
        server.publish_due(&connection).unwrap();
        let Message::Notification(diagnostics) = client.receiver.try_recv().unwrap() else {
            panic!("expected diagnostics")
        };
        assert_eq!(diagnostics.params["version"], 3);
        assert!(client.receiver.try_recv().is_err());
    }
}
