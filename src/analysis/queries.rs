//! Editor-independent language service results; the transport only serializes these.
use super::index::{Binding, DocumentId, Engine, Location};
use crate::{
    source::Position,
    syntax::{
        expression,
        lexer::Kind,
        model::{ScopeKind, SymbolKind},
    },
};
use serde_json::{Value, json};
use std::collections::HashSet;

impl Engine {
    pub fn definitions(&self, uri: &str, position: Position) -> Vec<Location> {
        self.target(uri, position)
            .into_iter()
            .map(|b| self.location(self.definition(b)))
            .collect()
    }
    pub fn references(
        &self,
        uri: &str,
        position: Position,
        include_declaration: bool,
        local_only: bool,
    ) -> Vec<Location> {
        let targets = self.target(uri, position);
        if targets.len() != 1 {
            return Vec::new();
        }
        let target = self.canonical(targets[0]);
        let name = self
            .document(target.document)
            .names
            .get(self.symbol(target).name);
        let mut locations = Vec::new();
        let mut seen = HashSet::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let doc = &entry.document;
            if local_only && doc.source.uri.as_ref() != uri {
                continue;
            }
            let Some(name_id) = doc.names.find(name) else {
                continue;
            };
            let id = DocumentId(i);
            if include_declaration {
                for (j, symbol) in doc.symbols.iter().enumerate() {
                    if !(j == 0 && symbol.kind == SymbolKind::Function)
                        && symbol.name == name_id
                        && self.same_symbol(
                            Binding {
                                document: id,
                                symbol: crate::syntax::model::SymbolId(j as u32),
                            },
                            target,
                        )
                        && seen.insert((i, symbol.span.start, symbol.span.end))
                    {
                        locations.push(self.span_location(id, symbol.span));
                    }
                }
            }
            for (j, token) in doc.tokens.iter().enumerate() {
                if token.kind != Kind::Name || token.name != name_id {
                    continue;
                }
                let declaration = self.declaration_at(id, token.span).is_some();
                if declaration && !include_declaration {
                    continue;
                }
                let resolved = self.resolve(id, j);
                if resolved.len() == 1
                    && self.same_symbol(resolved[0], target)
                    && seen.insert((i, token.span.start, token.span.end))
                {
                    locations.push(self.span_location(id, token.span));
                }
            }
        }
        locations.sort_by(|a, b| {
            (&a.uri, a.range.start.line, a.range.start.character).cmp(&(
                &b.uri,
                b.range.start.line,
                b.range.start.character,
            ))
        });
        locations.dedup();
        locations
    }
    pub fn signature_label(&self, owner: Binding) -> String {
        let symbol = self.symbol(owner);
        let args = symbol
            .parameters
            .iter()
            .map(|&id| {
                let p = self.document(owner.document).symbol(id);
                format!(
                    "{} = {}",
                    p.spelling,
                    p.ty.as_ref().map_or("?", |t| t.display.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}({}){}",
            symbol.spelling,
            args,
            symbol
                .ty
                .as_ref()
                .map_or(String::new(), |t| format!(" RETURNING {}", t.display))
        )
    }
    pub fn builtin_source(&self, uri: &str) -> Option<String> {
        let name = uri.strip_prefix("gorak-builtin:/")?.strip_suffix(".w4gl")?;
        let binding = super::builtins::binding(name)?;
        Some(self.document(binding.document).source.text().into())
    }
    pub fn code_lenses(&self, uri: &str) -> Value {
        let Some(id) = self.id(uri) else {
            return json!([]);
        };
        let doc = self.physical(id);
        if doc.component_kind != "classsource" {
            return json!([]);
        }
        let Some(symbol) = doc.symbols.first() else {
            return json!([]);
        };
        let range = doc.source.range(symbol.span);
        json!([{"range":range,"command":{"title":"Find class references","command":"gorak.findClassReferences","arguments":[uri,range.start]}}])
    }
    pub fn document_symbols(&self, uri: &str) -> Value {
        let Some(id) = self.id(uri) else {
            return json!([]);
        };
        let doc = self.physical(id);
        let symbols = doc
            .blocks
            .iter()
            .map(|block| {
                let mut scope = Some(block.scope);
                let mut fields = Vec::new();
                while let Some(id) = scope {
                    let current = &doc.scopes[id as usize];
                    if current.kind == ScopeKind::Field
                        && let Some(field) = doc.symbols.iter().find(|s| s.field_scope == Some(id))
                    {
                        fields.push(field.spelling.as_str());
                    }
                    scope = current.parent;
                }
                fields.reverse();
                json!({
                    "name":block.name,
                    "kind":block.kind,
                    "detail":fields.join(" / "),
                    "range":doc.source.range(block.span),
                    "selectionRange":doc.source.range(block.selection),
                })
            })
            .collect::<Vec<_>>();
        json!(symbols)
    }
    pub fn signature_help(&self, uri: &str, position: Position) -> Value {
        let Some(id) = self.id(uri) else {
            return Value::Null;
        };
        let doc = self.document(id);
        let byte = self.query_offset(id, position);
        let before = doc.tokens.partition_point(|t| t.span.start < byte);
        let Some(open) = self.call_open(id, before) else {
            return Value::Null;
        };
        let tree = expression::before(doc, open - 1);
        let Some(root) = tree.root else {
            return Value::Null;
        };
        let owners = self.bind_expression(id, &tree, root, 0);
        if owners.len() != 1 {
            return Value::Null;
        }
        let owner = owners[0];
        let declaration = self.symbol(owner);
        if !matches!(
            declaration.kind,
            SymbolKind::Method | SymbolKind::Function | SymbolKind::Frame
        ) {
            return Value::Null;
        }
        let mut active_label = None;
        let mut slot_start = open + 1;
        let mut nesting = 0;
        let mut slot_number = 0;
        let mut used = HashSet::new();
        for i in open + 1..doc.tokens.len() {
            let t = doc.tokens[i];
            if nesting == 0 && matches!(t.kind, Kind::Punct(b')' | b';' | b'{' | b'}')) {
                break;
            }
            if nesting == 0 && t.kind == Kind::Punct(b',') && t.span.end <= byte {
                slot_start = i + 1;
                slot_number += 1;
            }
            if nesting == 0
                && t.kind == Kind::Name
                && doc
                    .tokens
                    .get(i + 1)
                    .is_some_and(|n| n.kind == Kind::Punct(b'='))
            {
                used.insert(doc.names.get(t.name).to_owned());
            }
            if matches!(t.kind, Kind::Punct(b'(' | b'[')) {
                nesting += 1;
            }
            if matches!(t.kind, Kind::Punct(b')' | b']')) {
                nesting -= 1;
            }
        }
        if let Some(token) = doc.tokens.get(slot_start)
            && token.kind == Kind::Name
            && doc
                .tokens
                .get(slot_start + 1)
                .is_some_and(|n| n.kind == Kind::Punct(b'='))
        {
            active_label = Some(doc.names.get(token.name));
        }
        let parameters = &declaration.parameters;
        let owner_doc = self.document(owner.document);
        let active = active_label
            .and_then(|name| {
                parameters
                    .iter()
                    .position(|&p| owner_doc.names.get(owner_doc.symbol(p).name) == name)
            })
            .or_else(|| {
                if active_label.is_none() && used.is_empty() && !parameters.is_empty() {
                    return Some(slot_number.min(parameters.len() - 1));
                }
                if active_label.is_none() {
                    parameters.iter().position(|&p| {
                        !used.contains(owner_doc.names.get(owner_doc.symbol(p).name))
                    })
                } else {
                    None
                }
            });
        let mut signature = json!({"label":self.signature_label(owner),"documentation":"4GL parameters are named, may be supplied in any order, and may be omitted."});
        let mut result = json!({"activeSignature":0});
        if let Some(active) = active {
            signature["parameters"] = json!(
                parameters
                    .iter()
                    .map(|&p| json!({"label":owner_doc.symbol(p).spelling}))
                    .collect::<Vec<_>>()
            );
            result["activeParameter"] = json!(active);
        }
        result["signatures"] = json!([signature]);
        result
    }
    pub fn workspace_symbols(&self, query: &str) -> Value {
        let lower = query.to_ascii_lowercase();
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            for (j, s) in entry.document.symbols.iter().enumerate() {
                let b = Binding {
                    document: DocumentId(i),
                    symbol: crate::syntax::model::SymbolId(j as u32),
                };
                if (j == 0 && crate::syntax::is_wml(&entry.document.source.uri))
                    || !entry.document.names.get(s.name).contains(&lower)
                    || !seen.insert(self.canonical(b))
                {
                    continue;
                }
                result.push(json!({"name":s.spelling,"kind":s.kind.lsp(),"location":self.location(self.definition(b)),"containerName":entry.document.component}));
            }
        }
        json!(result)
    }
}

impl Engine {
    pub fn explain(&self, uri: &str, position: Position) -> Value {
        let Some(id) = self.id(uri) else {
            return json!({"status":"missing","candidates":[],"issues":["no-symbol"]});
        };
        let doc = self.document(id);
        let targets = self.target(uri, position);
        let Some(index) = doc.token_at(self.query_offset(id, position)) else {
            return if targets.is_empty() {
                self.explanation("missing", &targets, json!(["no-symbol"]))
            } else {
                self.explanation(
                    if targets.len() == 1 {
                        "resolved"
                    } else {
                        "ambiguous"
                    },
                    &targets,
                    json!([]),
                )
            };
        };
        let token = doc.tokens[index];
        let name = doc.names.get(token.name);
        let dynamic = index >= 2
            && doc.tokens[index - 1].kind == Kind::Punct(b':')
            && doc.tokens[index - 2].kind == Kind::Name
            && ["callproc", "callframe", "openframe", "gotoframe"]
                .contains(&doc.names.get(doc.tokens[index - 2].name));
        let qualifier = (index >= 2 && doc.tokens[index - 1].kind == Kind::Punct(b'!'))
            .then(|| doc.names.get(doc.tokens[index - 2].name));
        let app = super::index::application(uri);
        let lookup = self.graph.lookup(&app, qualifier, |_| Vec::<()>::new());
        let mut issues = if targets.is_empty() {
            serde_json::to_value(lookup.issues).unwrap_or(json!([]))
        } else {
            json!([])
        };
        if let Some(expansion) = &self.entry(id).expansion
            && targets.is_empty()
        {
            issues
                .as_array_mut()
                .unwrap()
                .extend(expansion.issues.iter().map(|s| json!(s)));
        }
        let builtin = targets.is_empty() && super::builtins::constant(name).is_some();
        let status = if dynamic {
            issues = json!(["runtime-target-unknown"]);
            "dynamic"
        } else if targets.len() > 1 {
            "ambiguous"
        } else if !targets.is_empty() {
            "resolved"
        } else if builtin {
            "builtin"
        } else if !issues.as_array().unwrap().is_empty() {
            "blocked"
        } else {
            "missing"
        };
        self.explanation(status, &targets, issues)
    }
    fn explanation(&self, status: &str, targets: &[Binding], issues: Value) -> Value {
        let candidates = targets
            .iter()
            .map(|&binding| {
                let location = self.location(binding);
                json!({
                    "id": format!("{}#{}", location.uri, binding.symbol.0),
                    "uri": location.uri,
                    "name": self.symbol(binding).spelling
                })
            })
            .collect::<Vec<_>>();
        json!({"status": status, "candidates": candidates, "issues": issues})
    }
}
