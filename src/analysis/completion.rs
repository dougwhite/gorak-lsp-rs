//! Completion candidates share the resolver's scope and visibility rules.
use super::{
    builtins,
    index::{Binding, DocumentId, Engine},
};
use crate::{
    source::{Position, Span},
    syntax::{
        expression, keywords,
        lexer::Kind,
        model::{SymbolId, SymbolKind},
    },
};
use serde_json::{Value, json};
use std::collections::HashSet;

struct Cursor {
    byte: u32,
    at: usize,
    partial: Option<usize>,
    prefix: String,
}
impl Engine {
    pub fn completions(&self, uri: &str, position: Position) -> Value {
        self.completions_with_snippets(uri, position, false)
    }
    pub fn completions_with_snippets(
        &self,
        uri: &str,
        position: Position,
        snippets: bool,
    ) -> Value {
        let Some(id) = self.id(uri) else {
            return json!([]);
        };
        let doc = self.document(id);
        let byte = self.query_offset(id, position);
        let boundary = doc.tokens.partition_point(|t| t.span.start < byte);
        let containing = boundary.checked_sub(1).map(|i| doc.tokens[i]);
        if containing.is_some_and(|t| t.kind == Kind::String && byte <= t.span.end) {
            return json!([]);
        }
        let gap_start = containing.map_or(0, |t| t.span.end.min(byte));
        let gap = doc.source.slice(Span {
            start: gap_start,
            end: byte,
        });
        let line = gap.rsplit(['\n', '\r']).next().unwrap_or_default();
        if line.contains("//")
            || line.contains("--")
            || gap
                .rfind("/*")
                .is_some_and(|start| !gap[start..].contains("*/"))
        {
            return json!([]);
        }
        let partial = boundary
            .checked_sub(1)
            .filter(|&i| doc.tokens[i].kind == Kind::Name && doc.tokens[i].span.end >= byte);
        let at = partial.unwrap_or(boundary);
        let prefix = partial.map_or(String::new(), |i| {
            doc.source
                .slice(Span {
                    start: doc.tokens[i].span.start,
                    end: byte,
                })
                .to_ascii_lowercase()
        });
        let cursor = Cursor {
            byte,
            at,
            partial,
            prefix,
        };
        if let Some(items) = self.argument_completions(id, &cursor) {
            return items;
        }
        let previous = at.checked_sub(1).map(|i| doc.tokens[i]);
        let member = previous.is_some_and(|t| t.kind == Kind::Punct(b'.'));
        let candidates = if member {
            self.receiver_completions(id, at)
        } else {
            self.scope_completions(id, containing.map_or(0, |t| t.scope))
        };
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for b in candidates {
            let symbol = self.symbol(b);
            let name = symbol.spelling.to_ascii_lowercase();
            if !name.starts_with(&cursor.prefix) || !seen.insert(name) {
                continue;
            }
            let mut item = json!({"label":symbol.spelling,"kind":match symbol.kind {SymbolKind::Method => 2,SymbolKind::Class => 7,_ => 6}});
            if let Some(ty) = &symbol.ty {
                item["detail"] = json!(ty.display);
            }
            if let Some(value) = &symbol.constant_value {
                item["detail"] = json!(format!(
                    "{} = {value}",
                    symbol
                        .ty
                        .as_ref()
                        .map_or("constant", |ty| ty.display.as_str())
                ));
            }
            items.push(item);
        }
        if !member {
            for name in builtins::class_names() {
                if name.to_ascii_lowercase().starts_with(&cursor.prefix)
                    && seen.insert(name.to_ascii_lowercase())
                {
                    items.push(json!({"label":name,"kind":7,"detail":"OpenROAD system class"}));
                }
            }
            for constant in builtins::constants() {
                if constant
                    .name
                    .to_ascii_lowercase()
                    .starts_with(&cursor.prefix)
                    && seen.insert(constant.name.to_ascii_lowercase())
                {
                    items.push(json!({"label":constant.name,"kind":21,"detail":format!("OpenROAD constant{}",constant.value.as_ref().map_or(String::new(),|v|format!(" = {v}"))),"documentation":{"kind":"markdown","value":format!("[OpenROAD 12.0 reference]({})",constant.url())}}));
                }
            }
            for &(name, ty) in builtins::CONTEXTS {
                if name.to_ascii_lowercase().starts_with(&cursor.prefix)
                    && seen.insert(name.to_ascii_lowercase())
                {
                    items.push(json!({"label":name,"kind":6,"detail":format!("OpenROAD context · {ty}"),"documentation":{"kind":"markdown","value":"[OpenROAD 12.0 reference](https://docs.actian.com/openroad/12.0/LangRef/Literals.htm)"}}));
                }
            }
            if snippets && self.call_open(id, cursor.at).is_none() {
                let line_start = doc.source.text()[..byte as usize]
                    .rfind('\n')
                    .map_or(0, |i| i + 1);
                if doc.source.text()[line_start..byte as usize]
                    .chars()
                    .all(|c| c.is_whitespace() || c.is_ascii_alphabetic() || c == '_')
                {
                    items.extend(blocks(&cursor.prefix));
                }
            }
            for word in keywords::WORDS
                .split_whitespace()
                .filter(|w| w.starts_with(&cursor.prefix))
            {
                items.push(json!({"label":word.to_ascii_uppercase(),"kind":14}));
            }
        }
        json!(items)
    }
    fn member_list(&self, class: Binding, seen: &mut HashSet<DocumentId>) -> Vec<Binding> {
        if !seen.insert(class.document) {
            return Vec::new();
        }
        let doc = self.document(class.document);
        let mut result: Vec<_> = doc
            .symbols
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != 0 && s.scope == 0)
            .map(|(i, _)| Binding {
                document: class.document,
                symbol: SymbolId(i as u32),
            })
            .collect();
        if let Some(parent) = &doc.superclass {
            let parents = self.components(class.document, parent);
            if parents.len() == 1 {
                result.extend(self.member_list(parents[0], seen));
            }
        }
        result
    }
    fn receiver_completions(&self, id: DocumentId, at: usize) -> Vec<Binding> {
        let Some(end) = at.checked_sub(2) else {
            return Vec::new();
        };
        let tree = expression::before(self.document(id), end);
        let Some(receiver) = tree
            .root
            .and_then(|root| self.expression_type(id, &tree, root, 0))
        else {
            return Vec::new();
        };
        if receiver.array {
            return builtins::binding("ArrayObject").map_or_else(Vec::new, |class| {
                self.member_list(class, &mut HashSet::new())
            });
        }
        if let Some((document, scope)) = receiver.fields {
            return self
                .document(document)
                .symbols
                .iter()
                .enumerate()
                .filter(|(_, s)| s.scope == scope)
                .map(|(i, _)| Binding {
                    document,
                    symbol: SymbolId(i as u32),
                })
                .collect();
        }
        if receiver.classes.len() == 1 {
            self.member_list(receiver.classes[0], &mut HashSet::new())
        } else {
            Vec::new()
        }
    }
    fn scope_completions(&self, id: DocumentId, scope: u32) -> Vec<Binding> {
        let doc = self.document(id);
        let mut candidates = Vec::new();
        let mut scope = Some(scope);
        let mut visited = HashSet::new();
        while let Some(s) = scope.filter(|s| visited.insert(*s)) {
            for (i, symbol) in doc
                .symbols
                .iter()
                .enumerate()
                .filter(|(_, symbol)| symbol.scope == s)
            {
                let _ = symbol;
                candidates.push(Binding {
                    document: id,
                    symbol: SymbolId(i as u32),
                });
            }
            scope = doc.scopes[s as usize].parent;
        }
        candidates.extend(self.member_list(
            Binding {
                document: id,
                symbol: SymbolId(0),
            },
            &mut HashSet::new(),
        ));
        let mut names = HashSet::new();
        for entry in &self.entries {
            if names.insert(&entry.document.component) {
                let found = self.components(id, &entry.document.component);
                if found.len() == 1 {
                    candidates.extend(found);
                }
            }
        }
        candidates
    }
    fn argument_completions(&self, id: DocumentId, cursor: &Cursor) -> Option<Value> {
        let doc = self.document(id);
        let open = self.call_open(id, cursor.at)?;
        let tree = expression::before(doc, open.checked_sub(1)?);
        let owners = self.bind_expression(id, &tree, tree.root?, 0);
        if owners.len() != 1 {
            return None;
        }
        let owner = self.symbol(owners[0]);
        if !matches!(
            owner.kind,
            SymbolKind::Method | SymbolKind::Function | SymbolKind::Frame
        ) {
            return None;
        }
        let mut depth = 0;
        let mut segment = open + 1;
        let mut used = HashSet::new();
        for i in open + 1..doc.tokens.len() {
            let t = doc.tokens[i];
            if depth == 0 {
                if matches!(t.kind, Kind::Punct(b')' | b';' | b'{' | b'}')) {
                    break;
                }
                if t.kind == Kind::Punct(b',') && t.span.end <= cursor.byte {
                    segment = i + 1;
                }
                if t.kind == Kind::Name
                    && doc
                        .tokens
                        .get(i + 1)
                        .is_some_and(|t| t.kind == Kind::Punct(b'='))
                    && cursor.partial != Some(i)
                {
                    used.insert(doc.names.get(t.name));
                }
            }
            match t.kind {
                Kind::Punct(b'(' | b'[') => depth += 1,
                Kind::Punct(b')' | b']') => depth -= 1,
                _ => {}
            }
        }
        if cursor.at > segment {
            return None;
        }
        let range = cursor.partial.map_or(
            Span {
                start: cursor.byte,
                end: cursor.byte,
            },
            |i| doc.tokens[i].span,
        );
        let spacing = if cursor.at.checked_sub(1).is_some_and(|i| {
            doc.tokens[i].kind == Kind::Punct(b',') && doc.tokens[i].span.end == range.start
        }) {
            " "
        } else {
            ""
        };
        let has_equals = cursor.partial.is_some_and(|i| {
            doc.tokens
                .get(i + 1)
                .is_some_and(|t| t.kind == Kind::Punct(b'='))
        });
        let owner_doc = self.document(owners[0].document);
        Some(json!(owner.parameters.iter().filter_map(|&p| {
            let parameter = owner_doc.symbol(p);
            let name = owner_doc.names.get(parameter.name);
            if used.contains(name) || !name.starts_with(&cursor.prefix) { return None; }
            Some(json!({"label":parameter.spelling,"kind":5,"detail":format!("{} · {}named parameter",parameter.ty.as_ref().map_or("?",|t|t.display.as_str()),if builtins::parameter_optional(owners[0],&parameter.spelling){"optional "}else{""}),"textEdit":{"range":self.span_location(id,range).range,"newText":format!("{}{}{}",spacing,parameter.spelling,if has_equals {""}else{" = "})}}))
        }).collect::<Vec<_>>()))
    }
}
fn blocks(prefix: &str) -> Vec<Value> {
    [
        ("IF","IF block","IF ${1:condition} THEN\n\t$0\nENDIF;"),
        ("IFELSE","IF / ELSE block","IF ${1:condition} THEN\n\t${2}\nELSE\n\t$0\nENDIF;"),
        ("WHILE","WHILE loop","WHILE ${1:condition} DO\n\t$0\nENDWHILE;"),
        ("FOR","FOR loop","FOR ${1:i} = ${2:1} TO ${3:limit} DO\n\t$0\nENDFOR;"),
        ("BEGIN","BEGIN / END block","BEGIN\n\t$0\nEND;"),
    ].into_iter().filter(|(word,_,_)|word.to_ascii_lowercase().starts_with(prefix))
    .map(|(word,detail,text)|json!({"label":format!("{word} block"),"filterText":word,"kind":15,"detail":detail,"insertText":text,"insertTextFormat":2})).collect()
}
