//! Rename produces edits only after proving static coverage and checking capture.
use super::{
    builtins,
    index::{Binding, DocumentId, Engine, Location, application},
};
use crate::{
    source::Position,
    syntax::{
        keywords,
        lexer::{Kind, Role},
        model::{SymbolId, SymbolKind},
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};
type RenameResult<T> = Result<T, String>;
fn project(uri: &str) -> String {
    let app = application(uri);
    Path::new(&app)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_string_lossy()
        .into_owned()
}
fn reserved(name: &str) -> bool {
    keywords::contains(name)||builtins::constant(name).is_some()||builtins::binding(name).is_some()||
        "integer integer1 integer2 integer4 integer8 int smallint float float4 float8 decimal money date ansidate timestamp varchar nvarchar char nchar text long byte boolean true false self curobject curframe curprocedure curmethod curexec cursession".split_whitespace().any(|n|n==name)
}
impl Engine {
    pub fn rename(&self, uri: &str, position: Position, new_name: &str) -> RenameResult<Value> {
        let targets = self.target(uri, position);
        if targets.len() != 1 {
            return Err("The symbol cannot be resolved unambiguously.".into());
        }
        let target = targets[0];
        let symbol = self.symbol(target);
        let name = symbol.spelling.to_ascii_lowercase();
        let lower = new_name.to_ascii_lowercase();
        if new_name.is_empty()
            || !new_name.as_bytes()[0].is_ascii_alphabetic() && new_name.as_bytes()[0] != b'_'
            || !new_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_#@$".contains(&b))
            || reserved(&lower)
        {
            return Err("Choose an OpenROAD identifier that is not a reserved word.".into());
        }
        if self
            .document(target.document)
            .source
            .uri
            .starts_with("gorak-builtin:")
            || builtins::documentation(target).is_some()
            || reserved(&name)
        {
            return Err(
                "Built-in declarations and keyword-spelled identifiers cannot be renamed.".into(),
            );
        }
        self.editable(target.document)?;
        let locations = if symbol.kind == SymbolKind::Method
            && self.document(target.document).component_kind == "classsource"
        {
            let family = self.method_family(target)?;
            self.method_locations(target, &family, &lower)?
        } else if symbol.parameter {
            self.parameter_locations(target, &lower)?
        } else if symbol.local && symbol.kind == SymbolKind::Variable {
            self.routine_safe(target)?;
            self.check_capture(target, &lower)?;
            self.references(uri, position, true, false)
        } else {
            return Err("This symbol kind does not yet have proven rename coverage.".into());
        };
        if locations.is_empty() {
            return Err("No complete references are available.".into());
        }
        let mut by_document: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut unique = HashSet::new();
        for location in locations {
            let key = (
                location.uri.clone(),
                location.range.start.line,
                location.range.start.character,
                location.range.end.line,
                location.range.end.character,
            );
            if unique.insert(key) {
                by_document
                    .entry(location.uri.to_string())
                    .or_default()
                    .push(json!({"range":location.range,"newText":new_name}));
            }
        }
        let changes = by_document
            .into_iter()
            .map(|(uri, edits)| {
                let version = self
                    .id(&uri)
                    .map(|id| self.entry(id).version)
                    .filter(|v| *v != 0);
                json!({"textDocument":{"uri":uri,"version":version},"edits":edits})
            })
            .collect::<Vec<_>>();
        Ok(json!({"documentChanges":changes}))
    }
    fn editable(&self, id: DocumentId) -> RenameResult<()> {
        let entry = self.entry(id);
        if entry.expansion.is_some() {
            return Err("Preprocessed source requires additional rename context.".into());
        }
        if !entry.document.errors.is_empty() {
            return Err("Fix syntax diagnostics before renaming.".into());
        }
        Ok(())
    }
    fn routine_safe(&self, target: Binding) -> RenameResult<()> {
        let doc = self.document(target.document);
        let symbol = self.symbol(target);
        if doc.tokens.iter().any(|t| {
            t.scope == symbol.scope
                && t.kind == Kind::Name
                && ["select", "insert", "update", "delete", "execute", "include"]
                    .contains(&doc.names.get(t.name))
        }) {
            return Err(
                "Rename is unavailable in routines containing SQL, dynamic execution or includes."
                    .into(),
            );
        }
        Ok(())
    }
    fn check_capture(&self, target: Binding, lower: &str) -> RenameResult<()> {
        let doc = self.document(target.document);
        let symbol = self.symbol(target);
        if doc.names.get(symbol.name) == lower {
            return Ok(());
        }
        if doc.tokens.iter().any(|t| {
            t.scope == symbol.scope && t.kind == Kind::Name && doc.names.get(t.name) == lower
        }) || doc
            .symbols
            .iter()
            .any(|s| s.scope == symbol.scope && doc.names.get(s.name) == lower)
        {
            return Err("The new name would collide with or capture an existing symbol.".into());
        }
        let mut scope = doc.scopes[symbol.scope as usize].parent;
        while let Some(s) = scope {
            if !self.scoped(target.document, s, lower).is_empty() {
                return Err("The new name would capture a name from an enclosing scope.".into());
            }
            scope = doc.scopes[s as usize].parent;
        }
        Ok(())
    }
    fn project_safe(&self, target: Binding) -> RenameResult<String> {
        let root = project(&self.document(target.document).source.uri);
        for app in self.graph.applications.values().filter(|app| {
            Path::new(&app.directory)
                .parent()
                .is_some_and(|p| p == Path::new(&root))
        }) {
            if app.includes.iter().any(|edge| edge.image.is_some())
                || !self
                    .graph
                    .lookup(&app.directory, None, |_| Vec::<()>::new())
                    .issues
                    .is_empty()
            {
                return Err(
                    "Rename requires complete application dependencies and valid metadata.".into(),
                );
            }
        }
        Ok(root)
    }
    fn ancestors(&self, id: DocumentId) -> RenameResult<Vec<DocumentId>> {
        let mut chain = Vec::new();
        let mut current = id;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current) {
                return Err("Rename cannot traverse cyclic inheritance.".into());
            }
            chain.push(current);
            let Some(parent) = &self.document(current).superclass else {
                break;
            };
            if parent.eq_ignore_ascii_case("userobject") {
                break;
            }
            let parents = self.components(current, parent);
            if parents.len() != 1
                || self.document(parents[0].document).component_kind != "classsource"
            {
                return Err("Rename requires all relevant superclass source.".into());
            }
            current = parents[0].document;
        }
        Ok(chain)
    }
    fn method_family(&self, target: Binding) -> RenameResult<HashSet<Binding>> {
        let root_project = self.project_safe(target)?;
        let name = self.symbol(target).spelling.to_ascii_lowercase();
        if builtins::binding("UserObject").is_some_and(|b| !self.members(b, &name).is_empty()) {
            return Err("System-method overrides cannot be renamed.".into());
        }
        let chain = self.ancestors(target.document)?;
        let root = *chain
            .iter()
            .rev()
            .find(|&&id| !self.scoped(id, 0, &name).is_empty())
            .unwrap_or(&target.document);
        if builtins::documentation(Binding {
            document: root,
            symbol: SymbolId(0),
        })
        .is_some()
        {
            return Err("System-method overrides cannot be renamed.".into());
        }
        let mut family = HashSet::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if self.interrupted() {
                return Err("Request interrupted".into());
            }
            if entry.document.component_kind != "classsource"
                || project(&entry.document.source.uri) != root_project
            {
                continue;
            }
            let id = DocumentId(i);
            let declarations = self.scoped(id, 0, &name);
            if declarations.is_empty() {
                continue;
            }
            if self.ancestors(id)?.contains(&root) {
                if declarations.len() != 1 {
                    return Err("The method declaration is ambiguous.".into());
                }
                family.insert(self.canonical(declarations[0]));
            }
        }
        Ok(family)
    }
    fn method_locations(
        &self,
        target: Binding,
        family: &HashSet<Binding>,
        new_name: &str,
    ) -> RenameResult<Vec<Location>> {
        let root = project(&self.document(target.document).source.uri);
        let name = self.symbol(target).spelling.to_ascii_lowercase();
        let mut result = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if self.interrupted() {
                return Err("Request interrupted".into());
            }
            let id = DocumentId(i);
            let doc = &entry.document;
            if project(&doc.source.uri) != root {
                continue;
            }
            if entry.dynamic_dispatch || entry.literal_names.contains(&name) {
                return Err(
                    "Dynamic method dispatch or a method-name string prevents proving coverage."
                        .into(),
                );
            }
            if entry
                .expansion
                .as_ref()
                .is_some_and(|e| !e.issues.is_empty())
            {
                return Err("Rename requires complete preprocessing.".into());
            }
            if self
                .members(
                    Binding {
                        document: id,
                        symbol: SymbolId(0),
                    },
                    &name,
                )
                .iter()
                .any(|b| family.contains(&self.canonical(*b)))
                && self
                    .members(
                        Binding {
                            document: id,
                            symbol: SymbolId(0),
                        },
                        new_name,
                    )
                    .iter()
                    .any(|b| !family.contains(&self.canonical(*b)))
            {
                return Err("The proposed name collides with an inherited member.".into());
            }
            for (j, t) in doc.tokens.iter().enumerate() {
                if j % 256 == 0 && self.interrupted() {
                    return Err("Request interrupted".into());
                }
                if t.kind == Kind::String
                    && doc
                        .source
                        .slice(t.span)
                        .trim_matches(['\'', '"'])
                        .eq_ignore_ascii_case(&name)
                    || t.kind == Kind::Punct(b':')
                        && j > 0
                        && doc.tokens[j - 1].kind == Kind::Punct(b'.')
                {
                    return Err("Dynamic method dispatch or a method-name string prevents proving coverage.".into());
                }
                if t.kind != Kind::Name || doc.names.get(t.name) != name || t.role == Role::Label {
                    continue;
                }
                let found = self.resolve(id, j);
                if found.is_empty()
                    && (j > 0 && doc.tokens[j - 1].kind == Kind::Punct(b'.')
                        || doc
                            .tokens
                            .get(j + 1)
                            .is_some_and(|t| t.kind == Kind::Punct(b'(')))
                {
                    return Err("An unresolved same-name call prevents a complete rename.".into());
                }
                if found.iter().any(|b| family.contains(&self.canonical(*b))) {
                    if found.len() != 1 {
                        return Err("Ambiguous method use prevents rename.".into());
                    }
                    self.editable(id)?;
                    result.push(self.span_location(id, t.span));
                }
            }
            for (j, s) in doc.symbols.iter().enumerate() {
                if family.contains(&self.canonical(Binding {
                    document: id,
                    symbol: SymbolId(j as u32),
                })) {
                    self.editable(id)?;
                    result.push(self.span_location(id, s.span));
                }
            }
        }
        result.sort_by_key(|l| (l.uri.clone(), l.range.start.line, l.range.start.character));
        Ok(result)
    }
    fn parameter_locations(&self, target: Binding, new_name: &str) -> RenameResult<Vec<Location>> {
        let doc = self.document(target.document);
        let parameter = self.symbol(target);
        let owner = Binding {
            document: target.document,
            symbol: parameter.owner.ok_or("Parameter owner is unavailable")?,
        };
        let declaration = self.symbol(owner);
        if declaration.kind == SymbolKind::Method && doc.component_kind == "classsource" {
            let family = self.method_family(owner)?;
            if family.len() != 1 {
                return Err(
                    "Parameter correspondence across method overrides is not verified.".into(),
                );
            }
            self.method_locations(owner, &family, &declaration.spelling.to_ascii_lowercase())?;
        } else if declaration.kind != SymbolKind::Function
            || doc.component_kind != "proc4glsource"
            || declaration.local
        {
            return Err(
                "Only global-procedure and non-overridden method parameters support rename.".into(),
            );
        }
        self.routine_safe(target)?;
        self.check_capture(target, new_name)?;
        let root = self.project_safe(target)?;
        let name = parameter.spelling.to_ascii_lowercase();
        let mut result = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if self.interrupted() {
                return Err("Request interrupted".into());
            }
            let id = DocumentId(i);
            let doc = &entry.document;
            if project(&doc.source.uri) != root {
                continue;
            }
            if entry.dynamic_calls {
                return Err("Dynamic calls prevent complete parameter rename.".into());
            }
            for (j, t) in doc.tokens.iter().enumerate() {
                if j % 256 == 0 && self.interrupted() {
                    return Err("Request interrupted".into());
                }
                let word = if t.kind == Kind::Name {
                    doc.names.get(t.name)
                } else {
                    ""
                };
                if word == "execute"
                    || t.kind == Kind::Punct(b':')
                        && j > 0
                        && (doc.tokens[j - 1].kind == Kind::Punct(b'.')
                            || ["callproc", "callframe", "openframe", "gotoframe"]
                                .contains(&doc.names.get(doc.tokens[j - 1].name)))
                {
                    return Err("Dynamic calls prevent complete parameter rename.".into());
                }
                if t.kind != Kind::Name || word != name {
                    continue;
                }
                let found = self.resolve(id, j);
                if t.role == Role::Argument && found.len() != 1 {
                    return Err("An unresolved same-name argument prevents complete rename.".into());
                }
                if found.iter().any(|&b| self.same_symbol(b, target)) {
                    if found.len() != 1 {
                        return Err("Ambiguous parameter use prevents rename.".into());
                    }
                    self.editable(id)?;
                    if new_name != name
                        && t.role == Role::Argument
                        && let Some(open) = self.call_open(id, j)
                    {
                        let call = crate::syntax::expression::parse(
                            doc,
                            open.saturating_sub(1),
                            doc.tokens.len(),
                        );
                        if call.nodes.iter().any(|node| match &node.kind {
                            crate::syntax::expression::ExprKind::Call { arguments, .. } => {
                                arguments.iter().any(|a| {
                                    a.label.is_some_and(|i| {
                                        doc.names.get(doc.tokens[i].name) == new_name
                                    })
                                })
                            }
                            _ => false,
                        }) {
                            return Err(
                                "A call already supplies an argument with the proposed name."
                                    .into(),
                            );
                        }
                    }
                    result.push(self.span_location(id, t.span));
                }
            }
        }
        Ok(result)
    }
}
