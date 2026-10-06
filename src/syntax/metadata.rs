//! TOML validates values; a separate physical-line pass locates authoring spans.
use super::{
    lexer::{Kind, Role, SyntaxError, Token},
    model::{Document, SymbolKind, TypeRef},
};
use crate::source::Span;

const COMPONENTS: &[&str] = &[
    "framesource",
    "frametemplate",
    "classsource",
    "proc4glsource",
    "proc3glsource",
    "globsource",
    "constsource",
    "scriptsource",
    "ghostsource",
];
pub fn type_ref(doc: &mut Document, text: &str) -> Option<TypeRef> {
    let mut value = text.trim();
    while let Some((first, tail)) = value.split_once(char::is_whitespace) {
        if ["returning", "private", "public"]
            .iter()
            .any(|x| first.eq_ignore_ascii_case(x))
        {
            value = tail.trim_start();
        } else {
            break;
        }
    }
    let element = value
        .split_once(char::is_whitespace)
        .filter(|(word, _)| word.eq_ignore_ascii_case("array"))
        .and_then(|(_, tail)| tail.trim_start().split_once(char::is_whitespace))
        .filter(|(word, _)| word.eq_ignore_ascii_case("of"))
        .map(|(_, tail)| tail.trim_start());
    let array = element.is_some();
    let value = element.unwrap_or(value);
    let name_end = value
        .find(|c: char| !c.is_ascii_alphanumeric() && !"_#@$!".contains(c))
        .unwrap_or(value.len());
    if name_end == 0 {
        return None;
    }
    let mut end = name_end;
    let remainder = &value[end..];
    let trimmed = remainder.trim_start();
    if trimmed.starts_with('(')
        && let Some(close) = trimmed.find(')')
    {
        end = value.len() - trimmed.len() + close + 1;
    }
    let display = format!(
        "{}{}",
        if array { "ARRAY OF " } else { "" },
        value[..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    );
    Some(TypeRef {
        name: doc.names.intern(&value[..name_end]),
        display,
        array,
    })
}
pub fn parse(doc: &mut Document, header: &str) {
    let value: toml::Value = match header.parse::<toml::Value>() {
        Ok(v) => v,
        Err(e) => {
            let span = e.span().unwrap_or(0..header.len().min(1));
            doc.errors.push(SyntaxError::new(
                Span::new(span.start, span.end),
                "invalid-toml",
                e.message(),
            ));
            return;
        }
    };
    let Some((kind, props)) = COMPONENTS
        .iter()
        .find_map(|&k| value.get(k).map(|p| (k, p)))
    else {
        doc.errors.push(SyntaxError::new(
            Span::new(0, 1.min(header.len())),
            "component-metadata",
            "Expected a gorak component metadata table.",
        ));
        return;
    };
    doc.component_kind = kind.into();
    doc.superclass = props
        .get("superclass")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let start = header.find(kind).unwrap_or(0);
    let component = doc.component.clone();
    let symbol_kind = match kind {
        "classsource" => SymbolKind::Class,
        "framesource" | "frametemplate" => SymbolKind::Frame,
        "globsource" => SymbolKind::Variable,
        "constsource" => SymbolKind::Constant,
        _ => SymbolKind::Function,
    };
    let id = doc.declare(
        &component,
        Span::new(start, start + kind.len()),
        0,
        symbol_kind,
    );
    doc.symbol_mut(id).ty = props
        .get("datatype")
        .and_then(toml::Value::as_str)
        .and_then(|s| type_ref(doc, s));
    if kind == "constsource" {
        // Character constants preserve empty strings and distinguish them from missing values.
        let character = doc.symbol(id).ty.as_ref().is_some_and(|ty| {
            matches!(
                doc.names.get(ty.name),
                "char" | "varchar" | "nchar" | "nvarchar" | "text" | "c"
            )
        });
        doc.symbol_mut(id).constant_value = props
            .get("defaultstring")
            .and_then(toml::Value::as_str)
            .map(|value| {
                if character {
                    format!("'{}'", value.replace('\'', "''"))
                } else {
                    value.to_owned()
                }
            })
            .or_else(|| {
                props
                    .get("defaultvalue")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            });
    }
    let mut section = "";
    let mut offset = 0;
    let mut multiline: Option<&str> = None;
    for line in header.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(quote) = multiline {
            if line.contains(quote) {
                multiline = None;
            }
            offset += line.len();
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed.trim_matches(['[', ']']);
        } else if let Some((key, rhs)) = line.split_once('=') {
            let key = key.trim().trim_matches(['\'', '"']);
            if ["attributes", "methods"].contains(&section)
                && let Some(raw) = value
                    .get(section)
                    .and_then(|v| v.get(key))
                    .and_then(toml::Value::as_str)
            {
                let start = offset + line.find(key).unwrap_or(0);
                let kind = if section == "methods" {
                    SymbolKind::Method
                } else {
                    SymbolKind::Property
                };
                let id = doc.declare(key, Span::new(start, start + key.len()), 0, kind);
                let datatype = if kind == SymbolKind::Method {
                    raw.to_ascii_lowercase()
                        .find("returning")
                        .map(|at| &raw[at + 9..])
                } else {
                    Some(raw)
                };
                let ty = datatype.and_then(|s| type_ref(doc, s));
                doc.symbol_mut(id).ty = ty;
                doc.tokens.push(Token {
                    span: doc.symbol(id).span,
                    kind: Kind::Name,
                    name: doc.symbol(id).name,
                    role: Role::Value,
                    scope: 0,
                });
                if let Some(datatype) = datatype {
                    add_type_token(doc, line, offset, datatype);
                }
            }
            if section == doc.component_kind
                && ["datatype", "superclass"].contains(&key)
                && let Some(datatype) = props.get(key).and_then(toml::Value::as_str)
            {
                add_type_token(doc, line, offset, datatype);
            }
            for quote in ["\"\"\"", "'''"] {
                let rhs = rhs.trim();
                if rhs.starts_with(quote) && !rhs[3..].contains(quote) {
                    multiline = Some(quote);
                }
            }
        }
        offset += line.len();
    }
}
fn add_type_token(doc: &mut Document, line: &str, offset: usize, datatype: &str) {
    let Some(ty) = type_ref(doc, datatype) else {
        return;
    };
    let Some(equal) = line.find('=') else {
        return;
    };
    let name = doc.names.get(ty.name);
    let Some(at) = line[equal + 1..].to_ascii_lowercase().find(name) else {
        return;
    };
    let start = offset + equal + 1 + at;
    if let Some((application, component)) = name.split_once('!') {
        let application = application.to_owned();
        let component = component.to_owned();
        let bang = start + application.len();
        let application_name = doc.names.intern(&application);
        let component_name = doc.names.intern(&component);
        doc.tokens.extend([
            Token {
                span: Span::new(start, bang),
                kind: Kind::Name,
                name: application_name,
                role: Role::Type,
                scope: 0,
            },
            Token {
                span: Span::new(bang, bang + 1),
                kind: Kind::Punct(b'!'),
                name: Default::default(),
                role: Role::Type,
                scope: 0,
            },
            Token {
                span: Span::new(bang + 1, bang + 1 + component.len()),
                kind: Kind::Name,
                name: component_name,
                role: Role::Type,
                scope: 0,
            },
        ]);
        return;
    }
    doc.tokens.push(Token {
        span: Span::new(start, start + name.len()),
        kind: Kind::Name,
        name: ty.name,
        role: Role::Type,
        scope: 0,
    });
}
