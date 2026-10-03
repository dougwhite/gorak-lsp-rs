//! XML is validated before script projection. Entity mappings retain authoring spans.
use super::{
    lexer::{Kind, Role, SyntaxError, Token},
    metadata,
    model::{Document, ScopeKind, SymbolKind},
    parser,
};
use crate::source::Span;
use std::collections::HashMap;

struct Piece {
    decoded: Span,
    original: Span,
    literal: bool,
}
#[derive(Default)]
struct Projection {
    text: String,
    pieces: Vec<Piece>,
    errors: Vec<SyntaxError>,
}
impl Projection {
    fn append(&mut self, text: &str, original: Span, literal: bool) {
        let start = self.text.len();
        self.text.push_str(text);
        self.pieces.push(Piece {
            decoded: Span::new(start, self.text.len()),
            original,
            literal,
        });
    }
    fn map(&self, span: Span) -> Span {
        let locate = |at: u32, end: bool| -> u32 {
            let target = if end { at.saturating_sub(1) } else { at };
            let index = self.pieces.partition_point(|p| p.decoded.end <= target);
            let piece = self.pieces.get(index);
            piece.map_or_else(
                || self.pieces.last().map_or(0, |p| p.original.end),
                |p| {
                    if p.literal {
                        p.original.start + at - p.decoded.start
                    } else if end {
                        p.original.end
                    } else {
                        p.original.start
                    }
                },
            )
        };
        Span {
            start: locate(span.start, false),
            end: locate(span.end, true),
        }
    }
}
fn project(raw: &str, base: usize) -> Projection {
    let mut p = Projection::default();
    let mut at = 0;
    while at < raw.len() {
        if raw[at..].starts_with("<![CDATA[") {
            let end = raw[at + 9..].find("]]>").map_or(raw.len(), |i| at + 9 + i);
            p.append(
                &raw[at + 9..end],
                Span::new(base + at + 9, base + end),
                true,
            );
            at = (end + 3).min(raw.len());
            continue;
        }
        if raw[at..].starts_with("<?") {
            let end = raw[at + 2..]
                .find("?>")
                .map_or(raw.len(), |i| at + 2 + i + 2);
            let instruction = &raw[at + 2..end.saturating_sub(2)];
            let mut parts = instruction.split_whitespace();
            if parts.next() == Some("ingres_invalidxmlchar") {
                let value = parts
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .and_then(char::from_u32);
                let span = Span::new(base + at, base + end);
                if let Some(value) = value.filter(|_| parts.next().is_none()) {
                    p.append(&value.to_string(), span, false);
                } else {
                    p.errors.push(SyntaxError::new(
                        span,
                        "invalid-wml-character",
                        "Expected one Unicode scalar code point",
                    ));
                    p.append(" ", span, false);
                }
            }
            at = end;
            continue;
        }
        if raw[at..].starts_with("<!--") {
            at = raw[at + 4..]
                .find("-->")
                .map_or(raw.len(), |i| at + 4 + i + 3);
            continue;
        }
        if raw.as_bytes()[at] == b'&'
            && let Some(end) = raw[at..].find(';')
        {
            let entity = &raw[at + 1..at + end];
            let ch = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "apos" => Some('\''),
                "quot" => Some('"'),
                "amp" => Some('&'),
                _ => entity
                    .strip_prefix("#x")
                    .and_then(|s| u32::from_str_radix(s, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|s| s.parse().ok()))
                    .and_then(char::from_u32),
            };
            if let Some(ch) = ch {
                p.append(
                    &ch.to_string(),
                    Span::new(base + at, base + at + end + 1),
                    false,
                );
                at += end + 1;
                continue;
            }
        }
        let end = raw[at..]
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '&' || *c == '<')
            .map_or(raw.len(), |(i, _)| at + i);
        p.append(&raw[at..end], Span::new(base + at, base + end), true);
        at = end;
    }
    p
}
pub fn parse(doc: &mut Document) {
    let source = doc.source.clone();
    let tree = match roxmltree::Document::parse(source.text()) {
        Ok(t) => t,
        Err(e) => {
            doc.errors.push(SyntaxError::new(
                Span::new(0, source.text().len().min(1)),
                "invalid-wml",
                e.to_string(),
            ));
            return;
        }
    };
    doc.component_kind = "framesource".into();
    let component = doc.component.clone();
    doc.declare(&component, Span::default(), 0, SymbolKind::Frame);
    let mut scopes = HashMap::new();
    for node in tree.descendants().filter(|n| n.is_element()) {
        let parent = node
            .parent()
            .and_then(|p| scopes.get(&p.id()).copied())
            .unwrap_or(0);
        let mut scope = parent;
        if let Some(attr) = node.attribute_node("gorak_style") {
            let range = attr.range();
            doc.errors.push(SyntaxError::new(
                Span::new(range.start, range.end), "obsolete-wml-style",
                "gorak_style is no longer supported; re-export this application with the current gorak CLI",
            ));
        }
        if is_field(node)
            && let Some(attr) = node.attribute_node("name")
        {
            let name = attr.value();
            if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                scope = doc.scope(parent, ScopeKind::Field);
                let range = attr.range_value();
                let span = Span::new(range.start, range.end);
                let id = doc.declare(name, span, parent, SymbolKind::Field);
                let mut ty = metadata::type_ref(doc, field_type(node));
                if node.tag_name().name().eq_ignore_ascii_case("tablefield")
                    && let Some(t) = &mut ty
                {
                    t.array = true;
                }
                let symbol = doc.symbol_mut(id);
                symbol.ty = ty;
                symbol.field_scope = Some(scope);
                let name_id = doc.names.intern(name);
                doc.tokens.push(Token {
                    span,
                    kind: Kind::Name,
                    name: name_id,
                    role: Role::Value,
                    scope: parent,
                });
            }
        }
        scopes.insert(node.id(), scope);
        if node.tag_name().name().eq_ignore_ascii_case("script") {
            let range = node.range();
            let raw = &source.text()[range.clone()];
            if let (Some(open), Some(close)) = (raw.find('>'), raw.rfind("</")) {
                let base = range.start + open + 1;
                let projection = project(&raw[open + 1..close], base);
                parser::parse_region(doc, &projection.text, parent, &|span| projection.map(span));
                doc.errors.extend(projection.errors);
            }
        }
    }
}
// Only native controls declare fields. Named tagged values, resources and other
// metadata are not variables; their names must never enter rename/reference sets.
fn is_field(node: roxmltree::Node<'_, '_>) -> bool {
    let tag = node.tag_name().name();
    tag.ends_with("field")
        || matches!(
            tag,
            "topform"
                | "subform"
                | "flexibleform"
                | "compositefield"
                | "freetrim"
                | "boxtrim"
                | "segmentshape"
                | "ellipseshape"
                | "rectangleshape"
                | "lineshape"
                | "menubar"
                | "menustack"
                | "menubutton"
                | "menutoggle"
                | "menuseparator"
        )
}

fn field_type<'a>(node: roxmltree::Node<'a, '_>) -> &'a str {
    if let Some(datatype) = node.attribute("datatype") {
        return datatype;
    }
    // A table column's value type belongs to its explicit prototype, not to the
    // creation palette. Neither fieldstyle nor stylesheet defaults infer types.
    if node.has_tag_name("columnfield")
        && let Some(prototype) = node.children().find(|n| n.has_tag_name("protofield"))
    {
        return prototype
            .attribute("datatype")
            .or_else(|| prototype.attribute("type"))
            .unwrap_or("formfield");
    }
    if node.has_tag_name("protofield") {
        return node.attribute("type").unwrap_or("formfield");
    }
    node.tag_name().name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;
    #[test]
    fn entities_and_cdata_keep_physical_locations() {
        let text = "<frame><entryfield name=\"amount\"><script><![CDATA[INITIALIZE = { }\nON CLICK = { }]]></script></entryfield><script>ON userevent &apos;save&apos; = { MESSAGE &apos;🎈&apos;; }</script></frame>";
        let doc = parser::parse(Source::new("file:///repo/app/main.wml", text).unwrap());
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[2].name, "ON userevent 'save'");
        assert_eq!(
            doc.source.slice(doc.blocks[2].selection),
            "ON userevent &apos;save&apos;"
        );
        assert_eq!(doc.source.slice(doc.blocks[1].span), "ON CLICK = { }");
    }
}
