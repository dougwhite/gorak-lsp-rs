//! Parsed TOML spans keep declarations separate from opaque member metadata.
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
    "extlibsource",
    "fieldtemplate",
];
fn type_text(text: &str) -> (&str, bool) {
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
    (element.unwrap_or(value), array)
}
pub fn type_ref(doc: &mut Document, text: &str) -> Option<TypeRef> {
    let (value, array) = type_text(text);
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
    let value = match toml_edit::ImDocument::parse(header) {
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
    // These catalogue components do not declare callable language symbols.
    if matches!(kind, "extlibsource" | "fieldtemplate") {
        return;
    }
    doc.superclass = props
        .get("superclass")
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned);
    let start = value
        .as_table()
        .key(kind)
        .and_then(|k| k.span())
        .map_or(0, |s| s.start);
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
        .and_then(toml_edit::Item::as_str)
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
            .and_then(toml_edit::Item::as_str)
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
                    .and_then(toml_edit::Item::as_str)
                    .map(str::to_owned)
            });
    }
    for section in ["attributes", "methods"] {
        let Some(members) = value.get(section).and_then(toml_edit::Item::as_table_like) else {
            continue;
        };
        for (name, member) in members.iter() {
            let declaration = if member.as_str().is_some() {
                Some(member)
            } else {
                member.get("declaration")
            };
            let Some(declaration) = declaration.filter(|d| d.as_str().is_some()) else {
                continue;
            };
            let Some(range) = members.key(name).and_then(|k| k.span()) else {
                continue;
            };
            let physical = &header[range.clone()];
            let bare = physical.trim_matches(['\'', '"']);
            if bare != name {
                doc.errors.push(SyntaxError::new(
                    Span::new(range.start, range.end),
                    "metadata-identifier",
                    "Escaped metadata identifiers do not support source-safe analysis.",
                ));
                continue;
            }
            let start = range.start + physical.find(bare).unwrap_or(0);
            let kind = if section == "methods" {
                SymbolKind::Method
            } else {
                SymbolKind::Property
            };
            let id = doc.declare(name, Span::new(start, start + name.len()), 0, kind);
            let raw = declaration.as_str().unwrap();
            // Defaults are not reference tokens, but a stored method name still
            // prevents proving rename coverage, just like a script literal.
            let lexed = super::lexer::lex(raw, 0, &mut doc.names);
            for token in lexed.tokens.iter().filter(|t| t.kind == Kind::String) {
                let literal = raw[token.span.start as usize..token.span.end as usize]
                    .trim_matches(['\'', '"']);
                if !literal.is_empty()
                    && literal
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_#@$".contains(&b))
                {
                    doc.facts.literal_names.insert(literal.to_ascii_lowercase());
                }
            }
            let datatype = if kind == SymbolKind::Method {
                raw.to_ascii_lowercase()
                    .find("returning")
                    .map(|at| &raw[at + 9..])
            } else {
                Some(raw)
            };
            doc.symbol_mut(id).ty = datatype.and_then(|s| type_ref(doc, s));
            doc.tokens.push(Token {
                span: doc.symbol(id).span,
                kind: Kind::Name,
                name: doc.symbol(id).name,
                role: Role::Value,
                scope: 0,
            });
            if let Some(datatype) = datatype {
                add_type_token(doc, header, declaration, datatype);
            }
        }
    }
    for key in ["datatype", "superclass"] {
        if let Some(item) = props.get(key)
            && let Some(datatype) = item.as_str()
        {
            add_type_token(doc, header, item, datatype);
        }
    }
}

fn add_type_token(doc: &mut Document, header: &str, item: &toml_edit::Item, datatype: &str) {
    let Some(ty) = type_ref(doc, datatype) else {
        return;
    };
    let Some(range) = item.span() else {
        return;
    };
    let raw = &header[range.clone()];
    let Some(decoded) = item.as_str() else {
        return;
    };
    // Match only the declaration prefix through the type. Escapes in a
    // default literal must not hide an otherwise literal type reference.
    let (type_text, _) = type_text(datatype);
    let trailing = decoded.len() - decoded.trim_end().len();
    let offset = decoded.len() - trailing - type_text.len();
    let name = doc.names.get(ty.name);
    let prefix = &decoded[..offset + name.len()];
    let quotes = if raw.starts_with("\"\"\"") || raw.starts_with("'''") {
        3
    } else {
        1
    };
    let mut content = quotes;
    if quotes == 3 {
        if raw[content..].starts_with("\r\n") {
            content += 2;
        } else if raw[content..].starts_with('\n') {
            content += 1;
        }
    }
    if !raw[content..].starts_with(prefix) {
        return;
    }
    let start = range.start + content + offset;
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
