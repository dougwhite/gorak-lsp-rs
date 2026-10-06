//! Error-tolerant routine/declaration parser. Statement expressions are parsed on demand.
use super::{
    lexer::{self, Kind, Role, SyntaxError, Token},
    metadata,
    model::*,
};
use crate::source::{Source, Span};

pub fn parse(source: Source) -> Document {
    let mut doc = Document::new(source.clone());
    if super::is_wml(&source.uri) {
        super::wml::parse(&mut doc);
    } else {
        let separator = source
            .text()
            .split_inclusive('\n')
            .scan(0, |at, line| {
                let start = *at;
                *at += line.len();
                Some((start, line))
            })
            .find(|(_, line)| line.trim() == "===");
        let header_end = separator.map_or(source.text().len(), |(start, _)| start);
        metadata::parse(&mut doc, &source.text()[..header_end]);
        if let Some((start, line)) = separator {
            let body_start = start + line.trim_end_matches(['\r', '\n']).len();
            parse_region(&mut doc, &source.text()[body_start..], 0, &|span| Span {
                start: span.start + body_start as u32,
                end: span.end + body_start as u32,
            });
        }
    }
    link_local_procedures(&mut doc);
    doc.tokens.sort_by_key(|t| t.span.start);
    let mut declared = std::collections::HashSet::new();
    for symbol in doc.symbols.iter().skip(1) {
        if !declared.insert((symbol.scope, symbol.name, symbol.implementation)) {
            doc.errors.push(SyntaxError::new(
                symbol.span,
                "duplicate-declaration",
                format!(
                    "Duplicate declaration of '{}' in this scope.",
                    symbol.spelling
                ),
            ));
        }
    }
    doc.facts = Facts::gather(&doc);
    doc
}
fn word<'a>(doc: &'a Document, t: &Token) -> &'a str {
    if t.kind == Kind::Name {
        doc.names.get(t.name)
    } else {
        ""
    }
}
fn punct(t: Option<&Token>, p: u8) -> bool {
    t.is_some_and(|t| t.kind == Kind::Punct(p))
}
fn is(doc: &Document, t: Option<&Token>, name: &str) -> bool {
    t.is_some_and(|t| word(doc, t) == name)
}
fn header(doc: &Document, tokens: &[Token], at: usize) -> bool {
    let current = word(doc, &tokens[at]);
    match current {
        "method" | "procedure" => {
            tokens.get(at + 1).is_some_and(|t| t.kind == Kind::Name)
                && punct(tokens.get(at + 2), b'(')
        }
        "initialize" => punct(tokens.get(at + 1), b'=') || punct(tokens.get(at + 1), b'('),
        "on" => tokens.get(at + 1).is_some_and(|t| t.kind == Kind::Name),
        _ => false,
    }
}
pub(super) fn parse_region(
    doc: &mut Document,
    text: &str,
    parent: u32,
    map: &dyn Fn(Span) -> Span,
) {
    let mut lexed = lexer::lex(text, 0, &mut doc.names);
    for token in lexed
        .tokens
        .iter()
        .filter(|token| token.kind == Kind::String)
    {
        let name =
            text[token.span.start as usize..token.span.end as usize].trim_matches(['\"', '\'']);
        if !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_#@$".contains(&byte))
        {
            doc.facts.literal_names.insert(name.to_ascii_lowercase());
        }
    }
    for e in &mut lexed.errors {
        e.span = map(e.span);
    }
    doc.errors.extend(lexed.errors);
    let tokens = &mut lexed.tokens;
    classify_sql(doc, tokens);
    let mut routines = Vec::new();
    let mut depth = 0;
    let mut declaration = false;
    for i in 0..tokens.len() {
        if tokens[i].kind == Kind::String || tokens[i].role == Role::Label {
            continue;
        }
        let name = word(doc, &tokens[i]);
        if punct(tokens.get(i), b'{') || name == "begin" {
            depth += 1;
            declaration = false;
        }
        if punct(tokens.get(i), b'}') || name == "end" {
            depth = (depth - 1).max(0);
        }
        let routine = header(doc, tokens, i);
        if depth != 0 {
            let line_start = text[..tokens[i].span.start as usize]
                .rfind('\n')
                .map_or(0, |p| p + 1);
            let recovery = routine
                && ["method", "procedure"].contains(&name)
                && text[line_start..tokens[i].span.start as usize]
                    .trim()
                    .is_empty();
            if !recovery {
                continue;
            }
            depth = 0;
        }
        if name == "declare" {
            declaration = true;
        }
        if name == "enddeclare" {
            declaration = false;
        }
        if !declaration
            && routine
            && !(name == "on" && i > 0 && tokens[i - 1].kind == Kind::Punct(b','))
        {
            routines.push(i);
        }
    }
    let shared = if doc.component_kind == "classsource" {
        doc.scopes
            .iter()
            .position(|s| s.kind == ScopeKind::ClassLocals)
            .map(|n| n as u32)
            .unwrap_or_else(|| doc.scope(parent, ScopeKind::ClassLocals))
    } else {
        parent
    };
    for (ordinal, &begin) in routines.iter().enumerate() {
        let end = routines.get(ordinal + 1).copied().unwrap_or(tokens.len());
        let kind = word(doc, &tokens[begin]).to_owned();
        let named = ["method", "procedure"].contains(&kind.as_str());
        let scope = doc.scope(shared, ScopeKind::Routine);
        for token in &mut tokens[begin..end] {
            token.scope = scope;
        }
        tokens[begin].role = Role::Label;
        let mut head_end = begin + 1;
        let mut nesting = 0;
        while head_end < end {
            let t = &tokens[head_end];
            if matches!(t.kind, Kind::Punct(b'(' | b'[')) {
                nesting += 1;
            }
            if matches!(t.kind, Kind::Punct(b')' | b']')) {
                nesting -= 1;
            }
            if nesting == 0
                && (matches!(t.kind, Kind::Punct(b'=' | b'{'))
                    || ["begin", "declare"].contains(&word(doc, t)))
            {
                break;
            }
            head_end += 1;
        }
        let selection = Span {
            start: tokens[begin].span.start,
            end: tokens[head_end.saturating_sub(1)].span.end,
        };
        let title = if named {
            format!(
                "{} {}()",
                kind.to_ascii_uppercase(),
                &text[tokens[begin + 1].span.start as usize..tokens[begin + 1].span.end as usize]
            )
        } else if kind == "on" {
            format!(
                "ON {}",
                text[tokens[begin + 1].span.start as usize..selection.end as usize].trim()
            )
        } else {
            "INITIALIZE".into()
        };
        doc.blocks.push(Block {
            name: title,
            span: map(Span {
                start: selection.start,
                end: tokens[end - 1].span.end,
            }),
            selection: map(selection),
            scope: parent,
            kind: if kind == "on" {
                24
            } else if kind == "method" {
                6
            } else {
                12
            },
        });
        let owner = if named {
            let t = tokens[begin + 1];
            let spelling = &text[t.span.start as usize..t.span.end as usize];
            let symbol_kind = if kind == "method" {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            let prior = doc
                .symbols
                .iter()
                .position(|d| {
                    !d.implementation
                        && d.name == t.name
                        && d.scope == parent
                        && d.kind == symbol_kind
                })
                .filter(|&prior| {
                    !doc.symbols
                        .iter()
                        .any(|s| s.implementation && s.canonical == doc.symbols[prior].canonical)
                });
            let id = doc.declare(spelling, map(t.span), parent, symbol_kind);
            doc.symbol_mut(id).implementation = true;
            if let Some(prior) = prior {
                let previous = doc.symbols[prior].clone();
                let d = doc.symbol_mut(id);
                d.canonical = previous.canonical;
                d.ty = previous.ty;
            }
            doc.scopes[scope as usize].owner = Some(id);
            Some(id)
        } else if kind == "initialize"
            && matches!(doc.component_kind.as_str(), "framesource" | "frametemplate")
        {
            Some(SymbolId(0))
        } else {
            None
        };
        parse_declarations(
            doc,
            text,
            tokens,
            DeclarationContext {
                start: begin + if named { 2 } else { 1 },
                end,
                routine_scope: scope,
                scope: if kind == "initialize" { shared } else { scope },
                owner,
            },
            map,
        );
        if kind == "on" {
            for token in &mut tokens[begin + 1..head_end] {
                token.role = Role::Label;
            }
        }
    }
    classify_arguments(doc, tokens);
    check_delimiters(doc, tokens, map);
    for mut token in lexed.tokens {
        token.span = map(token.span);
        doc.tokens.push(token);
    }
}
struct DeclarationContext {
    start: usize,
    end: usize,
    routine_scope: u32,
    scope: u32,
    owner: Option<SymbolId>,
}
fn parse_declarations(
    doc: &mut Document,
    text: &str,
    tokens: &mut [Token],
    context: DeclarationContext,
    map: &dyn Fn(Span) -> Span,
) {
    let DeclarationContext {
        start,
        end,
        routine_scope,
        scope,
        owner,
    } = context;
    let mut declare = false;
    let mut params = 0;
    let mut at = start;
    while at < end {
        if punct(tokens.get(at), b'{') || is(doc, tokens.get(at), "begin") {
            break;
        }
        if is(doc, tokens.get(at), "declare") {
            declare = true;
            params = 0;
            at += 1;
            continue;
        }
        if is(doc, tokens.get(at), "enddeclare") {
            declare = false;
            at += 1;
            continue;
        }
        if !declare && punct(tokens.get(at), b'(') {
            params += 1;
            at += 1;
            continue;
        }
        if !declare && punct(tokens.get(at), b')') {
            params = (params - 1).max(0);
            at += 1;
            continue;
        }
        let boundary = at == start
            || matches!(tokens[at - 1].kind, Kind::Punct(b'(' | b',' | b';'))
            || ["declare", "byref"].contains(&word(doc, &tokens[at - 1]));
        if !(declare || params > 0)
            || !boundary
            || tokens[at].kind != Kind::Name
            || !punct(tokens.get(at + 1), b'=')
            || tokens
                .get(at + 2)
                .is_none_or(|token| token.kind != Kind::Name)
        {
            at += 1;
            continue;
        }
        let local_procedure = is(doc, tokens.get(at + 2), "procedure");
        let mut type_start = at + 2;
        if local_procedure {
            type_start += 1;
            if is(doc, tokens.get(type_start), "returning") {
                type_start += 1;
            }
        }
        let mut type_end = type_start;
        let mut nesting = 0;
        while type_end < end {
            let token = tokens[type_end];
            if matches!(token.kind, Kind::Punct(b'(' | b'[')) {
                nesting += 1;
            }
            if matches!(token.kind, Kind::Punct(b')' | b']')) {
                if nesting == 0 {
                    break;
                }
                nesting -= 1;
            }
            if nesting == 0 && matches!(token.kind, Kind::Punct(b';' | b',' | b'{' | b'=')) {
                break;
            }
            if ["begin", "declare", "enddeclare"].contains(&word(doc, &token)) {
                break;
            }
            type_end += 1;
        }
        let ty = if type_end > type_start {
            let span = Span {
                start: tokens[type_start].span.start,
                end: tokens[type_end - 1].span.end,
            };
            metadata::type_ref(
                doc,
                &text[span.start as usize..span.end as usize].replace(" ! ", "!"),
            )
        } else {
            None
        };
        let token = tokens[at];
        let spelling = &text[token.span.start as usize..token.span.end as usize];
        let id = doc.declare(
            spelling,
            map(token.span),
            scope,
            if local_procedure {
                SymbolKind::Function
            } else {
                SymbolKind::Variable
            },
        );
        let symbol = doc.symbol_mut(id);
        symbol.ty = ty;
        symbol.local = scope == routine_scope;
        symbol.parameter = params > 0 && !declare;
        symbol.owner = if symbol.parameter { owner } else { None };
        if symbol.parameter
            && let Some(owner) = owner
        {
            doc.symbol_mut(owner).parameters.push(id);
        }
        for token in &mut tokens[type_start..type_end] {
            if token.kind == Kind::Name {
                token.role = Role::Type;
            }
        }
        at = type_end.max(at + 1);
    }
}
fn classify_sql(doc: &Document, tokens: &mut [Token]) {
    let mut sql = false;
    let mut host = false;
    let mut nesting = 0;
    for i in 0..tokens.len() {
        let word = word(doc, &tokens[i]);
        let previous = i.checked_sub(1).map(|p| tokens[p]);
        if [
            "select",
            "insert",
            "update",
            "delete",
            "fetch",
            "inquire_sql",
            "drop",
            "create",
            "alter",
            "grant",
            "revoke",
        ]
        .contains(&word)
            && previous.is_none_or(|t| {
                matches!(t.kind, Kind::Punct(b';' | b'{'))
                    || ["begin", "repeat"].contains(&super::parser::word(doc, &t))
            })
        {
            sql = true;
        }
        if matches!(tokens[i].kind, Kind::Punct(b'{' | b'}'))
            || matches!(word, "begin" | "end")
            || (matches!(word, "method" | "procedure") && header(doc, tokens, i))
        {
            sql = false;
            host = false;
        }
        if sql {
            if punct(tokens.get(i), b':') {
                host = true;
            } else if host {
                if matches!(tokens[i].kind, Kind::Punct(b'[' | b'(')) {
                    nesting += 1;
                }
                if matches!(tokens[i].kind, Kind::Punct(b']' | b')')) {
                    nesting = (nesting - 1).max(0);
                }
                if nesting == 0
                    && tokens[i].kind == Kind::Name
                    && previous.is_some_and(|t| t.kind == Kind::Name)
                {
                    host = false;
                }
                if nesting == 0
                    && !matches!(
                        tokens[i].kind,
                        Kind::Name | Kind::Punct(b'.' | b'!' | b'[' | b']' | b'(' | b')')
                    )
                {
                    host = false;
                }
            }
            if tokens[i].kind == Kind::Name && !host {
                tokens[i].role = Role::Label;
            }
        }
        if punct(tokens.get(i), b';') {
            sql = false;
            host = false;
            nesting = 0;
        }
    }
}
fn classify_arguments(doc: &Document, tokens: &mut [Token]) {
    let mut calls = Vec::new();
    for i in 0..tokens.len() {
        if punct(tokens.get(i), b'(') {
            let call = i > 0
                && (tokens[i - 1].kind == Kind::Name
                    && tokens[i - 1].role != Role::Type
                    && !is_keyword(word(doc, &tokens[i - 1]))
                    || matches!(tokens[i - 1].kind, Kind::Punct(b')' | b']')));
            calls.push(call);
        }
        if punct(tokens.get(i), b')') {
            calls.pop();
        }
        if calls.last() == Some(&true)
            && tokens[i].kind == Kind::Name
            && tokens[i].role == Role::Value
            && punct(tokens.get(i + 1), b'=')
            && i > 0
            && matches!(tokens[i - 1].kind, Kind::Punct(b'(' | b','))
        {
            tokens[i].role = Role::Argument;
        }
        if i > 0
            && ["callproc", "callframe", "openframe", "gotoframe"]
                .contains(&word(doc, &tokens[i - 1]))
        {
            tokens[i].role = Role::Call;
        }
    }
}
pub fn is_keyword(name: &str) -> bool {
    super::keywords::contains(name)
}
fn check_delimiters(doc: &mut Document, tokens: &[Token], map: &dyn Fn(Span) -> Span) {
    if doc.component_kind == "scriptsource" {
        return;
    }
    let mut stack: Vec<(&str, Span, String)> = Vec::new();
    for t in tokens {
        if t.kind == Kind::String || t.role == Role::Label {
            continue;
        }
        let marker = match t.kind {
            Kind::Punct(b'(') => "(",
            Kind::Punct(b')') => ")",
            Kind::Punct(b'[') => "[",
            Kind::Punct(b']') => "]",
            Kind::Punct(b'{') => "begin",
            Kind::Punct(b'}') => "end",
            _ => match word(doc, t) {
                "begin" => "begin",
                "end" => "end",
                "if" => "if",
                "endif" => "endif",
                _ => "",
            },
        };
        if ["(", "[", "begin", "if"].contains(&marker) {
            stack.push((marker, t.span, doc.source.slice(map(t.span)).into()));
        } else if let Some(open) = match marker {
            ")" => Some("("),
            "]" => Some("["),
            "end" => Some("begin"),
            "endif" => Some("if"),
            _ => None,
        } {
            if stack.last().is_some_and(|(m, _, _)| *m == open) {
                stack.pop();
            } else {
                doc.errors.push(SyntaxError::new(
                    map(t.span),
                    "unexpected-close",
                    format!("Unexpected '{}'.", doc.source.slice(map(t.span))),
                ));
            }
        }
    }
    let errors: Vec<_> = stack
        .into_iter()
        .map(|(_, span, spelling)| {
            SyntaxError::new(
                map(span),
                "unclosed-block",
                format!("Unclosed '{spelling}' block or delimiter."),
            )
        })
        .collect();
    doc.errors.extend(errors);
}
fn link_local_procedures(doc: &mut Document) {
    let declarations: Vec<_> = doc
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == SymbolKind::Function && s.local && !s.implementation)
        .map(|(i, _)| SymbolId(i as u32))
        .collect();
    for declared in declarations {
        let declaration = doc.symbol(declared).clone();
        let bodies: Vec<_> = doc
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name == declaration.name && s.implementation)
            .map(|(i, _)| SymbolId(i as u32))
            .collect();
        if bodies.len() != 1
            || doc
                .symbols
                .iter()
                .filter(|s| s.name == declaration.name && s.local && !s.implementation)
                .count()
                != 1
        {
            continue;
        }
        let body = bodies[0];
        let parameters = doc.symbol(body).parameters.clone();
        let d = doc.symbol_mut(body);
        d.canonical = declared;
        d.scope = declaration.scope;
        d.ty = declaration.ty;
        d.local = true;
        doc.symbol_mut(declared).parameters = parameters;
        if let Some(scope) = doc.scopes.iter_mut().find(|s| s.owner == Some(body)) {
            scope.parent = Some(declaration.scope);
        }
    }
}
