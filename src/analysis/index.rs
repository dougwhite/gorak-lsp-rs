//! Symbol identity and lookup are shared by every language service.
use crate::{
    project::Graph,
    source::{Position, Range, Source, Span},
    syntax::{
        self,
        expression::{self, ExprId, ExprKind, Expressions},
        lexer::{Kind, NameId, Role},
        model::{Document, Symbol, SymbolId, SymbolKind},
    },
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub document: DocumentId,
    pub symbol: SymbolId,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Location {
    pub uri: Arc<str>,
    pub range: Range,
}
pub struct Entry {
    pub document: Document,
    pub version: i32,
    pub physical: Option<Document>,
    pub expansion: Option<crate::preprocessor::Expansion>,
    pub requires_preprocessing: bool,
    pub dynamic_calls: bool,
    pub dynamic_dispatch: bool,
    pub literal_names: HashSet<String>,
    scopes: HashMap<(u32, NameId), Vec<SymbolId>>,
    declarations: HashMap<u32, (u32, SymbolId)>,
    implementations: HashMap<SymbolId, Vec<SymbolId>>,
}
impl Entry {
    pub(super) fn new(document: Document, version: i32) -> Self {
        let mut scopes: HashMap<_, Vec<_>> = HashMap::new();
        for (i, s) in document.symbols.iter().enumerate() {
            scopes
                .entry((s.scope, s.name))
                .or_default()
                .push(SymbolId(i as u32));
        }
        let declarations = document
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (s.span.start, (s.span.end, SymbolId(i as u32))))
            .collect();
        let mut implementations: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for (i, s) in document
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.implementation)
        {
            implementations
                .entry(s.canonical)
                .or_default()
                .push(SymbolId(i as u32));
        }
        Self {
            dynamic_calls: document.facts.dynamic_calls,
            dynamic_dispatch: document.facts.dynamic_dispatch,
            literal_names: document.facts.literal_names.clone(),
            requires_preprocessing: document.facts.requires_preprocessing,
            declarations,
            implementations,
            document,
            version,
            scopes,
            physical: None,
            expansion: None,
        }
    }
}
#[derive(Default)]
pub struct Engine {
    pub graph: Graph,
    pub entries: Vec<Entry>,
    uris: HashMap<String, DocumentId>,
    components: HashMap<(String, String), Vec<DocumentId>>,
    dirty_expansions: HashSet<DocumentId>,
    preprocessed: HashSet<DocumentId>,
    pub resident_detail_bytes: usize,
    pub(crate) changed_details: Vec<Arc<str>>,
}
#[derive(Clone)]
pub struct Receiver {
    pub classes: Vec<Binding>,
    pub array: bool,
    pub fields: Option<(DocumentId, u32)>,
}
pub fn application(uri: &str) -> String {
    crate::source::uri_path(uri)
        .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_default()
}
pub fn component_key(uri: &str) -> &str {
    uri.rsplit_once('.')
        .filter(|(_, extension)| {
            extension.eq_ignore_ascii_case("w4gl") || extension.eq_ignore_ascii_case("wml")
        })
        .map_or(uri, |(stem, _)| stem)
}

impl Engine {
    pub fn update(
        &mut self,
        uri: &str,
        text: &str,
        version: i32,
    ) -> Result<DocumentId, &'static str> {
        self.insert(syntax::parse(Source::new(uri, text)?), version)
    }
    pub fn insert(&mut self, document: Document, version: i32) -> Result<DocumentId, &'static str> {
        let uri = document.source.uri.to_string();
        let created = !self.uris.contains_key(&uri);
        for &id in &self.preprocessed {
            if created
                || self
                    .entry(id)
                    .expansion
                    .as_ref()
                    .is_some_and(|e| e.dependencies.contains(uri.as_str()))
            {
                self.dirty_expansions.insert(id);
            }
        }
        let app = application(&uri);
        self.graph.ensure(app.clone());
        let key = (app, document.component.to_ascii_lowercase());
        let id = if let Some(&id) = self.uris.get(&uri) {
            let old = &self.entries[id.0].document;
            let old_key = (application(&uri), old.component.to_ascii_lowercase());
            if let Some(ids) = self.components.get_mut(&old_key) {
                ids.retain(|&i| i != id);
            }
            self.resident_detail_bytes = self
                .resident_detail_bytes
                .saturating_sub(self.detail_bytes(id));
            self.entries[id.0] = Entry::new(document, version);
            id
        } else {
            let id = DocumentId(self.entries.len());
            self.entries.push(Entry::new(document, version));
            self.uris.insert(uri, id);
            id
        };
        self.resident_detail_bytes += self.detail_bytes(id);
        self.changed_details
            .push(self.document(id).source.uri.clone());
        self.components.entry(key).or_default().push(id);
        if self.entries[id.0].requires_preprocessing {
            self.preprocessed.insert(id);
            self.dirty_expansions.insert(id);
        } else {
            self.preprocessed.remove(&id);
            self.dirty_expansions.remove(&id);
        }
        Ok(id)
    }
    pub fn remove(&mut self, uri: &str) {
        let Some(id) = self.uris.remove(uri) else {
            return;
        };
        self.resident_detail_bytes = self
            .resident_detail_bytes
            .saturating_sub(self.detail_bytes(id));
        self.entries.swap_remove(id.0);
        self.components.clear();
        self.preprocessed.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            let doc = &entry.document;
            if entry.requires_preprocessing {
                self.preprocessed.insert(DocumentId(i));
            }
            self.uris.insert(doc.source.uri.to_string(), DocumentId(i));
            self.components
                .entry((
                    application(&doc.source.uri),
                    doc.component.to_ascii_lowercase(),
                ))
                .or_default()
                .push(DocumentId(i));
        }
        self.invalidate_expansions();
    }
    /// Rebuild expanded syntax at a request boundary, after the latest graph and source updates.
    pub fn prepare(&mut self) {
        let dirty = std::mem::take(&mut self.dirty_expansions);
        for id in dirty {
            if !self.entry(id).requires_preprocessing {
                continue;
            }
            let raw = self.physical(id);
            let keep_details = raw.source.is_resident();
            let Some(root) = resident_source(&raw.source) else {
                continue;
            };
            let app = application(&raw.source.uri);
            let app_name = std::path::Path::new(&app)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let expansion = crate::preprocessor::expand(&root, &app_name, &raw.component, |name| {
                let (qualifier, name) = name
                    .split_once('!')
                    .map_or((None, name), |(app, name)| (Some(app), name));
                let lookup = self.graph.lookup(&app, qualifier, |app| {
                    self.components
                        .get(&(app.into(), name.to_ascii_lowercase()))
                        .cloned()
                        .unwrap_or_default()
                });
                if lookup.candidates.len() != 1 {
                    return None;
                }
                let file = self.physical(lookup.candidates[0]);
                (file.component_kind == "scriptsource")
                    .then(|| resident_source(&file.source))
                    .flatten()
            });
            let Ok(source) = Source::new(raw.source.uri.clone(), expansion.text.as_str()) else {
                continue;
            };
            let document = syntax::parse(source);
            self.resident_detail_bytes = self
                .resident_detail_bytes
                .saturating_sub(self.detail_bytes(id));
            let entry = &mut self.entries[id.0];
            let mut replacement = Entry::new(document, entry.version);
            replacement.physical = Some(
                entry
                    .physical
                    .take()
                    .unwrap_or_else(|| entry.document.clone()),
            );
            replacement.requires_preprocessing = entry.requires_preprocessing;
            replacement.expansion = Some(expansion);
            *entry = replacement;
            self.resident_detail_bytes += self.detail_bytes(id);
            self.changed_details
                .push(self.document(id).source.uri.clone());
            if !keep_details {
                self.evict(id);
            }
        }
    }
    pub fn invalidate_expansions(&mut self) {
        self.dirty_expansions.clone_from(&self.preprocessed);
    }
    pub fn hydrate(&mut self, document: Document) -> Result<DocumentId, &'static str> {
        let uri = document.source.uri.clone();
        let Some(id) = self.id(&uri) else {
            return self.insert(document, 0);
        };
        if self.physical(id).source.fingerprint != document.source.fingerprint {
            return self.insert(document, 0);
        }
        self.resident_detail_bytes = self
            .resident_detail_bytes
            .saturating_sub(self.detail_bytes(id));
        let entry = &mut self.entries[id.0];
        if entry.physical.is_some() {
            entry.physical = Some(document);
            self.dirty_expansions.insert(id);
        } else {
            entry.document = document;
        }
        self.resident_detail_bytes += self.detail_bytes(id);
        self.changed_details
            .push(self.document(id).source.uri.clone());
        Ok(id)
    }
    pub fn evict(&mut self, id: DocumentId) {
        self.resident_detail_bytes = self
            .resident_detail_bytes
            .saturating_sub(self.detail_bytes(id));
        let entry = &mut self.entries[id.0];
        release_document(&mut entry.document);
        if let Some(physical) = &mut entry.physical {
            release_document(physical);
        }
        if let Some(expansion) = &mut entry.expansion {
            expansion.text.clear();
            expansion.text.shrink_to_fit();
            for origin in &mut expansion.origins {
                origin.source.release_text();
            }
        }
    }
    pub fn detail_bytes(&self, id: DocumentId) -> usize {
        let entry = &self.entries[id.0];
        let bytes = |doc: &Document| {
            doc.source.resident_bytes()
                + doc.tokens.capacity() * std::mem::size_of::<crate::syntax::lexer::Token>()
        };
        bytes(&entry.document)
            + entry.physical.as_ref().map_or(0, bytes)
            + entry.expansion.as_ref().map_or(0, |e| e.text.capacity())
    }
    pub fn physical(&self, id: DocumentId) -> &Document {
        let entry = self.entry(id);
        entry.physical.as_ref().unwrap_or(&entry.document)
    }
    pub fn query_offset(&self, id: DocumentId, position: Position) -> u32 {
        let raw = self.physical(id);
        let byte = raw.source.offset(position);
        self.entry(id)
            .expansion
            .as_ref()
            .and_then(|map| map.offset(&raw.source.uri, byte))
            .unwrap_or(byte)
    }
    pub fn span_location(&self, id: DocumentId, span: Span) -> Location {
        if let Some((source, span)) = self
            .entry(id)
            .expansion
            .as_ref()
            .and_then(|map| map.location(span))
        {
            return Location {
                uri: source.uri.clone(),
                range: source.range(span),
            };
        }
        let doc = self.document(id);
        Location {
            uri: doc.source.uri.clone(),
            range: doc.source.range(span),
        }
    }
    pub fn same_symbol(&self, a: Binding, b: Binding) -> bool {
        self.canonical(a) == self.canonical(b)
            || self.location(self.canonical(a)) == self.location(self.canonical(b))
    }
    pub fn id(&self, uri: &str) -> Option<DocumentId> {
        self.uris
            .get(uri)
            .or_else(|| self.uris.get(&crate::source::canonical_uri(uri)))
            .copied()
    }
    pub fn entry(&self, id: DocumentId) -> &Entry {
        self.entries
            .get(id.0)
            .or_else(|| super::builtins::entry(id))
            .expect("binding refers to an indexed document")
    }
    pub fn document(&self, id: DocumentId) -> &Document {
        &self.entry(id).document
    }
    pub fn symbol(&self, b: Binding) -> &Symbol {
        self.document(b.document).symbol(b.symbol)
    }
    pub fn canonical(&self, b: Binding) -> Binding {
        if let Some(origin) = super::builtins::canonical(b) {
            return origin;
        }
        Binding {
            document: b.document,
            symbol: self.document(b.document).canonical(b.symbol),
        }
    }
    fn unique(&self, items: Vec<Binding>) -> Vec<Binding> {
        let mut slots = HashMap::new();
        let mut result: Vec<Binding> = Vec::new();
        for item in items {
            let key = self.canonical(item);
            if let Some(&slot) = slots.get(&key) {
                if self.symbol(item).implementation {
                    result[slot] = item;
                }
            } else {
                slots.insert(key, result.len());
                result.push(item);
            }
        }
        result
    }
    pub fn components(&self, from: DocumentId, name: &str) -> Vec<Binding> {
        let (qualifier, name) = name
            .split_once('!')
            .map_or((None, name), |(q, n)| (Some(q), n));
        let app = application(&self.document(from).source.uri);
        let result = self.graph.lookup(&app, qualifier, |app| {
            let mut found = self
                .components
                .get(&(app.to_owned(), name.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default();
            // W4GL and WML are two authoring views of one frame, not ambiguous components.
            found.sort_by_key(|&id| syntax::is_wml(&self.document(id).source.uri));
            let mut seen = HashSet::new();
            found
                .into_iter()
                .filter(|&id| !self.document(id).symbols.is_empty())
                .filter(|&id| seen.insert(component_key(&self.document(id).source.uri).to_owned()))
                .map(|document| Binding {
                    document,
                    symbol: SymbolId(0),
                })
                .collect()
        });
        if result.candidates.is_empty() && result.issues.is_empty() && qualifier.is_none() {
            return super::builtins::binding(name).into_iter().collect();
        }
        result
            .candidates
            .into_iter()
            .map(|b| self.definition(b))
            .collect()
    }
    pub fn scoped(&self, document: DocumentId, scope: u32, name: &str) -> Vec<Binding> {
        let doc = self.document(document);
        let Some(name) = doc.names.find(name) else {
            return Vec::new();
        };
        self.unique(
            self.entry(document)
                .scopes
                .get(&(scope, name))
                .into_iter()
                .flatten()
                .map(|&symbol| Binding { document, symbol })
                .collect(),
        )
    }
    pub fn members(&self, class: Binding, name: &str) -> Vec<Binding> {
        self.members_inner(class, name, &mut HashSet::new())
    }
    fn members_inner(
        &self,
        class: Binding,
        name: &str,
        seen: &mut HashSet<DocumentId>,
    ) -> Vec<Binding> {
        if !seen.insert(class.document) {
            return Vec::new();
        }
        let doc = self.document(class.document);
        let own = self.scoped(class.document, 0, name);
        if !own.is_empty() {
            return own;
        }
        if let Some(parent) = &doc.superclass {
            let parents = self.components(class.document, parent);
            if parents.len() == 1 {
                return self.members_inner(parents[0], name, seen);
            }
        }
        Vec::new()
    }
    pub fn resolve(&self, document: DocumentId, token: usize) -> Vec<Binding> {
        self.resolve_inner(document, token, 0)
    }
    fn resolve_inner(&self, id: DocumentId, index: usize, depth: u16) -> Vec<Binding> {
        if depth > 32 {
            return Vec::new();
        }
        let doc = self.document(id);
        let Some(token) = doc.tokens.get(index) else {
            return Vec::new();
        };
        if token.role == Role::Label {
            return Vec::new();
        }
        if let Some(symbol) = self.declaration_at(id, token.span) {
            return vec![Binding {
                document: id,
                symbol,
            }];
        }
        let name = doc.names.get(token.name);
        let previous = index.checked_sub(1).map(|i| doc.tokens[i]);
        if token.role == Role::Argument {
            if let Some(open) = self.call_open(id, index) {
                let tree = expression::before(doc, open - 1);
                if let Some(root) = tree.root {
                    let owners = self.bind_expression(id, &tree, root, depth + 1);
                    if owners.len() == 1 {
                        return self
                            .symbol(owners[0])
                            .parameters
                            .iter()
                            .filter_map(|&parameter| {
                                let p = Binding {
                                    document: owners[0].document,
                                    symbol: parameter,
                                };
                                (self.document(p.document).names.get(self.symbol(p).name) == name)
                                    .then_some(p)
                            })
                            .collect();
                    }
                }
            }
            return Vec::new();
        }
        if previous.is_some_and(|t| t.kind == Kind::Punct(b'!')) && index >= 2 {
            return self.components(
                id,
                &format!("{}!{name}", doc.names.get(doc.tokens[index - 2].name)),
            );
        }
        if doc
            .tokens
            .get(index + 1)
            .is_some_and(|t| t.kind == Kind::Punct(b'!'))
        {
            return Vec::new();
        }
        if token.role == Role::Type {
            return self.components(id, name);
        }
        if previous.is_some_and(|t| t.kind == Kind::Punct(b'.')) && index >= 2 {
            let tree = expression::before(doc, index - 2);
            if let Some(root) = tree.root {
                return self.receiver_members(id, &tree, root, name, depth + 1);
            }
            return Vec::new();
        }
        if syntax::parser::is_keyword(name) && name != "method" {
            return Vec::new();
        }
        let mut scope = Some(token.scope);
        let mut visited = HashSet::new();
        while let Some(current) = scope {
            if !visited.insert(current) {
                break;
            }
            let candidates = self.scoped(id, current, name);
            if !candidates.is_empty() {
                return candidates;
            }
            scope = doc.scopes.get(current as usize).and_then(|s| s.parent);
        }
        let members = self.members(
            Binding {
                document: id,
                symbol: SymbolId(0),
            },
            name,
        );
        if !members.is_empty() {
            return members;
        }
        // Frame fields may live in the sibling WML authoring document.
        if let Some(siblings) = self.components.get(&(
            application(&doc.source.uri),
            doc.component.to_ascii_lowercase(),
        )) {
            for &sibling in siblings {
                if sibling != id {
                    let found = self.scoped(sibling, 0, name);
                    if !found.is_empty() {
                        return found;
                    }
                }
            }
        }
        if let Some(class) = match name {
            "curframe" => Some("FrameExec"),
            "curprocedure" => Some("ProcExec"),
            "curexec" => Some(match doc.component_kind.as_str() {
                "framesource" => "FrameExec",
                "classsource" => "MethodExec",
                _ => "ProcExec",
            }),
            "cursession" => Some("SessionObject"),
            "cureventscope" | "curscriptscope" => Some("Scope"),
            "curmethod" => Some("MethodExec"),
            "curfield" => Some("FormField"),
            "curevent" => Some("Event"),
            _ => None,
        } {
            return super::builtins::binding(class).into_iter().collect();
        }
        self.components(id, name)
    }
    pub fn call_open(&self, id: DocumentId, before: usize) -> Option<usize> {
        let doc = self.document(id);
        let mut depth = 0;
        for i in (0..before).rev() {
            match doc.tokens[i].kind {
                Kind::Punct(b')' | b']') => depth += 1,
                Kind::Punct(b'(' | b'[') => {
                    if depth > 0 {
                        depth -= 1;
                    } else if doc.tokens[i].kind == Kind::Punct(b'(') && i > 0 {
                        let previous = doc.tokens[i - 1];
                        if matches!(previous.kind, Kind::Punct(b')' | b']'))
                            || previous.kind == Kind::Name
                                && (!syntax::parser::is_keyword(doc.names.get(previous.name))
                                    || doc.names.get(previous.name) == "method")
                        {
                            return Some(i);
                        }
                    }
                }
                Kind::Punct(b';' | b'{' | b'}') if depth == 0 => break,
                _ => {}
            }
        }
        None
    }
    pub fn bind_expression(
        &self,
        id: DocumentId,
        tree: &Expressions,
        expr: ExprId,
        depth: u16,
    ) -> Vec<Binding> {
        if depth > 64 {
            return Vec::new();
        }
        let doc = self.document(id);
        match tree.get(expr).kind {
            ExprKind::Name(token) => self.resolve_inner(id, token, depth + 1),
            ExprKind::Qualified { application, name } => self.components(
                id,
                &format!(
                    "{}!{}",
                    doc.names.get(doc.tokens[application].name),
                    doc.names.get(doc.tokens[name].name)
                ),
            ),
            ExprKind::Member { receiver, name } => self.receiver_members(
                id,
                tree,
                receiver,
                doc.names.get(doc.tokens[name].name),
                depth + 1,
            ),
            _ => Vec::new(),
        }
    }
    fn receiver_members(
        &self,
        id: DocumentId,
        tree: &Expressions,
        receiver: ExprId,
        name: &str,
        depth: u16,
    ) -> Vec<Binding> {
        let Some(value) = self.expression_type(id, tree, receiver, depth + 1) else {
            return Vec::new();
        };
        if let Some((document, scope)) = value.fields
            && !value.array
        {
            return self.scoped(document, scope, name);
        }
        if value.array {
            let arrays = self.components(id, "ArrayObject");
            return arrays
                .first()
                .map_or_else(Vec::new, |&b| self.members(b, name));
        }
        if value.classes.len() == 1 {
            self.members(value.classes[0], name)
        } else {
            Vec::new()
        }
    }
    pub fn expression_type(
        &self,
        id: DocumentId,
        tree: &Expressions,
        expr: ExprId,
        depth: u16,
    ) -> Option<Receiver> {
        if depth > 64 {
            return None;
        }
        let doc = self.document(id);
        let target = match &tree.get(expr).kind {
            ExprKind::Group(inner) | ExprKind::Unary(inner) => {
                return self.expression_type(id, tree, *inner, depth + 1);
            }
            ExprKind::Index { receiver } => {
                let mut value = self.expression_type(id, tree, *receiver, depth + 1)?;
                if !value.array {
                    return None;
                }
                value.array = false;
                return Some(value);
            }
            ExprKind::Name(token) => {
                if ["self", "curobject"].contains(&doc.names.get(doc.tokens[*token].name)) {
                    return Some(Receiver {
                        classes: vec![Binding {
                            document: id,
                            symbol: SymbolId(0),
                        }],
                        array: false,
                        fields: None,
                    });
                }
                expr
            }
            ExprKind::Call { callee, .. } => *callee,
            _ => expr,
        };
        let owners = self.bind_expression(id, tree, target, depth + 1);
        if owners.len() != 1 {
            return None;
        }
        let owner = owners[0];
        let symbol = self.symbol(owner);
        if let ExprKind::Call { arguments, .. } = &tree.get(expr).kind
            && symbol.kind == SymbolKind::Class
            && (!tree.get(expr).complete || arguments.len() != 1 || arguments[0].label.is_some())
        {
            return None;
        }
        if matches!(tree.get(expr).kind, ExprKind::Call { .. }) && symbol.kind == SymbolKind::Class
        {
            return Some(Receiver {
                classes: vec![owner],
                array: false,
                fields: None,
            });
        }
        if symbol.kind == SymbolKind::Class
            && matches!(tree.get(expr).kind, ExprKind::Name(token) if ["curframe", "curprocedure", "curexec", "curmethod", "curfield", "curevent"].contains(&doc.names.get(doc.tokens[token].name)))
        {
            return Some(Receiver {
                classes: vec![owner],
                array: false,
                fields: None,
            });
        }
        let ty = symbol.ty.as_ref()?;
        let owner_doc = self.document(owner.document);
        let classes = self.components(owner.document, owner_doc.names.get(ty.name));
        Some(Receiver {
            classes,
            array: ty.array,
            fields: symbol.field_scope.map(|scope| (owner.document, scope)),
        })
    }
    pub fn target(&self, uri: &str, position: Position) -> Vec<Binding> {
        let Some(id) = self.id(uri) else {
            return Vec::new();
        };
        let doc = self.document(id);
        let byte = self.query_offset(id, position);
        if doc.symbols.first().is_some_and(|s| s.span.contains(byte)) {
            return vec![Binding {
                document: id,
                symbol: SymbolId(0),
            }];
        }
        doc.token_at(byte)
            .map_or_else(Vec::new, |i| self.resolve(id, i))
    }
    pub fn location(&self, b: Binding) -> Location {
        self.span_location(b.document, self.symbol(b).span)
    }
    pub fn declaration_at(&self, id: DocumentId, span: Span) -> Option<SymbolId> {
        self.entry(id)
            .declarations
            .get(&span.start)
            .filter(|(end, _)| *end == span.end)
            .map(|(_, id)| *id)
    }
    pub fn definition(&self, b: Binding) -> Binding {
        let canonical = self.document(b.document).canonical(b.symbol);
        self.entry(b.document)
            .implementations
            .get(&canonical)
            .filter(|ids| ids.len() == 1)
            .map_or(b, |ids| Binding {
                document: b.document,
                symbol: ids[0],
            })
    }
}

fn resident_source(source: &Source) -> Option<Source> {
    if source.is_resident() {
        return Some(source.clone());
    }
    let path = url::Url::parse(&source.uri).ok()?.to_file_path().ok()?;
    Source::new(source.uri.clone(), std::fs::read_to_string(path).ok()?).ok()
}
fn release_document(document: &mut Document) {
    document.source.release_text();
    document.tokens.clear();
    document.tokens.shrink_to_fit();
}
