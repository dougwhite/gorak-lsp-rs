//! Allocation-free token text: names are interned once per document; literals keep spans.
use crate::source::Span;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap, sync::Arc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NameId(pub u32);
#[derive(Clone, Debug, Default)]
pub struct Names {
    values: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, NameId>,
}
impl Serialize for Names {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.values.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Names {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<Arc<str>>::deserialize(deserializer)?;
        let ids = values
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), NameId(i as u32)))
            .collect();
        Ok(Self { values, ids })
    }
}
impl Names {
    pub fn intern(&mut self, value: &str) -> NameId {
        let lower = fold(value);
        if let Some(&id) = self.ids.get(lower.as_ref()) {
            return id;
        }
        let id = NameId(self.values.len() as u32);
        let value: Arc<str> = lower.into_owned().into();
        self.values.push(value.clone());
        self.ids.insert(value, id);
        id
    }
    pub fn get(&self, id: NameId) -> &str {
        &self.values[id.0 as usize]
    }
    pub fn find(&self, text: &str) -> Option<NameId> {
        self.ids.get(fold(text).as_ref()).copied()
    }
}
fn fold(text: &str) -> Cow<'_, str> {
    if text.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(text.to_ascii_lowercase())
    } else {
        Cow::Borrowed(text)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Name,
    Number,
    String,
    Punct(u8),
    Operator,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[default]
    Value,
    Type,
    Label,
    Call,
    Argument,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Token {
    pub span: Span,
    pub kind: Kind,
    pub name: NameId,
    pub role: Role,
    pub scope: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntaxError {
    pub span: Span,
    pub code: String,
    pub message: String,
}
impl SyntaxError {
    pub fn new(span: Span, code: &str, message: impl Into<String>) -> Self {
        Self {
            span,
            code: code.into(),
            message: message.into(),
        }
    }
}
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<SyntaxError>,
}
fn name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn name_part(b: u8) -> bool {
    name_start(b) || b.is_ascii_digit() || b"#@$".contains(&b)
}
fn space(c: char) -> bool {
    c.is_whitespace() && c != '\u{85}' || c == '\u{feff}'
}
pub fn lex(text: &str, base: u32, names: &mut Names) -> Lexed {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    while at < bytes.len() {
        let start = at;
        let b = bytes[at];
        if b.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if b >= 128 {
            let c = text[at..].chars().next().unwrap();
            if space(c) {
                at += c.len_utf8();
                continue;
            }
        }
        if bytes[at..].starts_with(b"//") || bytes[at..].starts_with(b"--") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if bytes[at..].starts_with(b"/*") {
            if let Some(end) = text[at + 2..].find("*/") {
                at += end + 4;
                continue;
            }
            errors.push(SyntaxError::new(
                Span::new(start + base as usize, start + base as usize + 2),
                "unterminated-comment",
                "Unterminated block comment.",
            ));
            break;
        }
        let kind;
        let mut name = NameId::default();
        if b == b'\'' || b == b'"' {
            kind = Kind::String;
            at += 1;
            let mut closed = false;
            while at < bytes.len() {
                if bytes[at] == b {
                    if bytes.get(at + 1) == Some(&b) {
                        at += 2;
                        continue;
                    }
                    at += 1;
                    closed = true;
                    break;
                }
                at += 1;
            }
            if !closed {
                errors.push(SyntaxError::new(
                    Span::new(start + base as usize, start + base as usize + 1),
                    "unterminated-string",
                    "Unterminated quoted string.",
                ));
            }
        } else if name_start(b) {
            kind = Kind::Name;
            at += 1;
            while at < bytes.len() && name_part(bytes[at]) {
                at += 1;
            }
            name = names.intern(&text[start..at]);
        } else if b.is_ascii_digit() {
            kind = Kind::Number;
            at += 1;
            while at < bytes.len() && bytes[at].is_ascii_digit() {
                at += 1;
            }
            if bytes.get(at) == Some(&b'.') {
                at += 1;
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            if matches!(bytes.get(at), Some(b'e' | b'E')) {
                let mut end = at + 1;
                if matches!(bytes.get(end), Some(b'+' | b'-')) {
                    end += 1;
                }
                let first = end;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > first {
                    at = end;
                }
            }
        } else if bytes.get(at..at + 2).is_some_and(|s| {
            [b"**", b"<=", b">=", b"!=", b"<>", b"^=", b":="].contains(&s.try_into().unwrap())
        }) {
            kind = Kind::Operator;
            at += 2;
        } else {
            kind = Kind::Punct(b);
            at += text[at..].chars().next().unwrap().len_utf8();
        }
        tokens.push(Token {
            span: Span::new(start + base as usize, at + base as usize),
            kind,
            name,
            role: Role::Value,
            scope: 0,
        });
    }
    Lexed { tokens, errors }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scientific_numbers_strings_and_comments() {
        let mut names = Names::default();
        let out = lex("Name = 1e-3; 'a''B🎈' /* x */ NAME", 0, &mut names);
        assert!(out.errors.is_empty());
        assert_eq!(out.tokens.len(), 6);
        assert_eq!(out.tokens[0].name, out.tokens[5].name);
        assert_eq!(out.tokens[4].kind, Kind::String);
    }
    #[test]
    fn invalid_literal_recovers_without_losing_location() {
        let out = lex("/* broken", 7, &mut Names::default());
        assert_eq!(out.errors[0].span, Span::new(7, 9));
    }
}
