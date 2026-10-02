//! Conservative advisory checks: unknown types never become compiler errors.
use super::{
    builtins,
    index::{Binding, DocumentId, Engine, Receiver},
};
use crate::{
    source::Span,
    syntax::{
        expression::{self, ExprKind, Expressions},
        lexer::{Kind, Role},
    },
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
struct ReferenceType {
    name: String,
    identity: DocumentId,
    ancestors: HashSet<DocumentId>,
}
impl Engine {
    pub fn diagnostics(&self, uri: &str) -> Value {
        let Some(id) = self.id(uri) else {
            return json!([]);
        };
        let doc = self.document(id);
        let mut diagnostics = Vec::new();
        for error in &doc.errors {
            let location = self.span_location(id, error.span);
            if location.uri.as_ref() == uri {
                diagnostics.push(json!({"range":location.range,"message":error.message,"severity":1,"source":"gorak","code":error.code}));
            }
        }
        let mut calls: HashMap<usize, HashSet<String>> = HashMap::new();
        for (i, t) in doc.tokens.iter().enumerate() {
            if t.role == Role::Label {
                continue;
            }
            if t.role == Role::Argument && self.declaration_at(id, t.span).is_none() {
                if let Some(open) = self.call_open(id, i) {
                    let tree = expression::before(doc, open - 1);
                    let owners = tree
                        .root
                        .map_or_else(Vec::new, |root| self.bind_expression(id, &tree, root, 0));
                    if owners.len() != 1 {
                        continue;
                    }
                    let owner = owners[0];
                    if !self.document(owner.document).errors.is_empty() {
                        continue;
                    }
                    let method = self.symbol(owner);
                    let name = doc.names.get(t.name);
                    if !calls.entry(open).or_default().insert(name.into()) {
                        self.warning(
                            id,
                            t.span,
                            "duplicate-argument",
                            format!(
                                "Parameter '{}' is supplied more than once.",
                                doc.source.slice(t.span)
                            ),
                            &mut diagnostics,
                        );
                    }
                    let owner_doc = self.document(owner.document);
                    let parameter = method
                        .parameters
                        .iter()
                        .find(|&&p| owner_doc.names.get(owner_doc.symbol(p).name) == name);
                    if parameter.is_none() && builtins::signature_complete(owner) {
                        self.warning(
                            id,
                            t.span,
                            "unknown-argument",
                            format!(
                                "'{}' has no declared parameter '{}'.",
                                method.spelling,
                                doc.source.slice(t.span)
                            ),
                            &mut diagnostics,
                        );
                    }
                    if let Some(&parameter) = parameter {
                        let rhs = expression::parse(doc, i + 2, doc.tokens.len());
                        if let Some(expected) = self.binding_type(Binding {
                            document: owner.document,
                            symbol: parameter,
                        }) {
                            self.compare(
                                id,
                                &rhs,
                                expected,
                                &format!("Argument '{}'", doc.source.slice(t.span)),
                                &mut diagnostics,
                            );
                        }
                    }
                }
            } else if t.kind == Kind::Punct(b'=')
                && i > 0
                && doc.tokens[i - 1].role != Role::Argument
                && !doc.symbols.iter().any(|s| s.span == doc.tokens[i - 1].span)
            {
                let lhs = expression::before(doc, i - 1);
                let rhs = expression::parse(doc, i + 1, doc.tokens.len());
                if let Some(root) = lhs
                    .root
                    .filter(|&root| self.assignment_start(id, lhs.get(root).span.start))
                    && let Some(expected) = self.expression_type(id, &lhs, root, 0)
                {
                    let context = if matches!(lhs.get(root).kind, ExprKind::Index { .. }) {
                        "Array element assignment"
                    } else {
                        "Assignment"
                    };
                    self.compare(id, &rhs, expected, context, &mut diagnostics);
                }
            } else if t.kind == Kind::Name && doc.names.get(t.name) == "return" {
                let rhs = expression::parse(doc, i + 1, doc.tokens.len());
                if let Some(owner) = doc.scopes[t.scope as usize].owner
                    && let Some(expected) = self.binding_type(Binding {
                        document: id,
                        symbol: owner,
                    })
                {
                    self.compare(id, &rhs, expected, "Return value", &mut diagnostics);
                }
            }
        }
        json!(diagnostics)
    }
    fn assignment_start(&self, id: DocumentId, start: u32) -> bool {
        let doc = self.document(id);
        let first = doc.tokens.partition_point(|t| t.span.start < start);
        let Some(previous) = first.checked_sub(1).map(|i| doc.tokens[i]) else {
            return true;
        };
        matches!(previous.kind, Kind::Punct(b';' | b'{' | b'}'))
            || previous.kind == Kind::Name
                && ["begin", "then", "else", "do", "enddeclare"]
                    .contains(&doc.names.get(previous.name))
    }
    fn binding_type(&self, binding: Binding) -> Option<Receiver> {
        let doc = self.document(binding.document);
        let ty = self.symbol(binding).ty.as_ref()?;
        Some(Receiver {
            classes: self.components(binding.document, doc.names.get(ty.name)),
            array: ty.array,
            fields: None,
        })
    }
    fn compare(
        &self,
        id: DocumentId,
        tree: &Expressions,
        expected: Receiver,
        context: &str,
        diagnostics: &mut Vec<Value>,
    ) {
        let Some(root) = tree.root.filter(|&root| tree.get(root).complete) else {
            return;
        };
        let Some(actual) = self
            .expression_type(id, tree, root, 0)
            .and_then(|value| self.reference_type(value))
        else {
            return;
        };
        let Some(expected) = self.reference_type(expected) else {
            return;
        };
        if expected.identity != actual.identity
            && !expected.ancestors.contains(&actual.identity)
            && !actual.ancestors.contains(&expected.identity)
        {
            self.warning(
                id,
                tree.get(root).span,
                "incompatible-reference-type",
                format!(
                    "{context}: '{}' and '{}' are unrelated reference types.",
                    actual.name, expected.name
                ),
                diagnostics,
            );
        }
    }
    fn reference_type(&self, value: Receiver) -> Option<ReferenceType> {
        if value.classes.len() != 1 {
            return None;
        }
        let element = value.classes[0];
        let mut class = if value.array {
            builtins::binding("ArrayObject")?
        } else {
            element
        };
        let name = format!(
            "{}{}",
            if value.array { "ARRAY OF " } else { "" },
            self.document(element.document).component
        );
        let identity = class.document;
        let mut ancestors = HashSet::new();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(class.document) {
                return None;
            }
            if let Some(names) = builtins::ancestors(class.document) {
                for name in names {
                    ancestors.insert(builtins::binding(name)?.document);
                }
                break;
            }
            let Some(parent) = &self.document(class.document).superclass else {
                break;
            };
            let parents = self.components(class.document, parent);
            if parents.len() != 1 {
                return None;
            }
            class = parents[0];
            ancestors.insert(class.document);
        }
        Some(ReferenceType {
            name,
            identity,
            ancestors,
        })
    }
    fn warning(
        &self,
        id: DocumentId,
        span: Span,
        code: &str,
        message: String,
        diagnostics: &mut Vec<Value>,
    ) {
        let location = self.span_location(id, span);
        if location.uri != self.physical(id).source.uri {
            return;
        }
        diagnostics.push(json!({"range":location.range,"code":code,"message":message,"severity":2,"source":"gorak-semantic"}));
    }
}
