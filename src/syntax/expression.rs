//! Small, on-demand expression arenas. Incomplete input keeps useful receiver structure.
use super::{
    lexer::{Kind, Token},
    model::Document,
    parser::is_keyword,
};
use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(pub usize);
#[derive(Clone, Debug)]
pub struct Argument {
    pub label: Option<usize>,
    pub value: Option<ExprId>,
}
#[derive(Clone, Debug)]
pub enum ExprKind {
    Name(usize),
    Qualified {
        application: usize,
        name: usize,
    },
    Literal(usize),
    Member {
        receiver: ExprId,
        name: usize,
    },
    Call {
        callee: ExprId,
        arguments: Vec<Argument>,
    },
    Index {
        receiver: ExprId,
    },
    Group(ExprId),
    Unary(ExprId),
    Binary {
        left: ExprId,
        right: ExprId,
    },
}
#[derive(Clone, Debug)]
pub struct Expression {
    pub kind: ExprKind,
    pub span: Span,
    pub complete: bool,
}
#[derive(Default, Debug)]
pub struct Expressions {
    pub nodes: Vec<Expression>,
    pub root: Option<ExprId>,
    pub next: usize,
}
impl Expressions {
    pub fn get(&self, id: ExprId) -> &Expression {
        &self.nodes[id.0]
    }
}
struct Parser<'a> {
    doc: &'a Document,
    at: usize,
    end: usize,
    tree: Expressions,
}
impl Parser<'_> {
    fn token(&self) -> Option<Token> {
        (self.at < self.end).then(|| self.doc.tokens[self.at])
    }
    fn punct(&self, c: u8) -> bool {
        self.token().is_some_and(|t| t.kind == Kind::Punct(c))
    }
    fn name(&self, t: Token) -> &str {
        if t.kind == Kind::Name {
            self.doc.names.get(t.name)
        } else {
            ""
        }
    }
    fn push(&mut self, kind: ExprKind, span: Span, complete: bool) -> ExprId {
        let id = ExprId(self.tree.nodes.len());
        self.tree.nodes.push(Expression {
            kind,
            span,
            complete,
        });
        id
    }
    fn parse(&mut self, minimum: u8, depth: u16) -> Option<ExprId> {
        if depth > 128 {
            return None;
        }
        let token = self.token()?;
        let start = token.span.start;
        let index = self.at;
        let mut left;
        if self.name(token) == "callproc" {
            self.at += 1;
            left = self.parse(7, depth + 1)?;
            if matches!(
                self.tree.get(left).kind,
                ExprKind::Name(_) | ExprKind::Qualified { .. }
            ) {
                let span = self.tree.get(left).span;
                left = self.push(
                    ExprKind::Call {
                        callee: left,
                        arguments: Vec::new(),
                    },
                    Span {
                        start,
                        end: span.end,
                    },
                    true,
                );
            }
        } else if matches!(token.kind, Kind::Punct(b'+' | b'-' | b':')) || self.name(token) == "not"
        {
            self.at += 1;
            let right = self.parse(if self.name(token) == "not" { 3 } else { 6 }, depth + 1)?;
            let child = self.tree.get(right);
            left = self.push(
                ExprKind::Unary(right),
                Span {
                    start,
                    end: child.span.end,
                },
                child.complete,
            );
        } else if self.punct(b'(') {
            self.at += 1;
            let inner = self.parse(0, depth + 1)?;
            let closed = self.punct(b')');
            let end = if closed {
                let e = self.doc.tokens[self.at].span.end;
                self.at += 1;
                e
            } else {
                self.tree.get(inner).span.end
            };
            left = self.push(
                ExprKind::Group(inner),
                Span { start, end },
                closed && self.tree.get(inner).complete,
            );
        } else if token.kind == Kind::Name {
            self.at += 1;
            if self.punct(b'!')
                && self
                    .doc
                    .tokens
                    .get(self.at + 1)
                    .is_some_and(|t| t.kind == Kind::Name)
            {
                let name = self.at + 1;
                self.at += 2;
                left = self.push(
                    ExprKind::Qualified {
                        application: index,
                        name,
                    },
                    Span {
                        start,
                        end: self.doc.tokens[name].span.end,
                    },
                    true,
                );
            } else {
                left = self.push(ExprKind::Name(index), token.span, true);
            }
        } else if matches!(token.kind, Kind::String | Kind::Number) {
            self.at += 1;
            left = self.push(ExprKind::Literal(index), token.span, true);
        } else {
            return None;
        }
        while let Some(operator) = self.token() {
            if self.punct(b'.') {
                self.at += 1;
                if self.token().is_some_and(|t| t.kind == Kind::Name) {
                    let name = self.at;
                    self.at += 1;
                    left = self.push(
                        ExprKind::Member {
                            receiver: left,
                            name,
                        },
                        Span {
                            start,
                            end: self.doc.tokens[name].span.end,
                        },
                        self.tree.get(left).complete,
                    );
                } else {
                    left = self.push(
                        ExprKind::Member {
                            receiver: left,
                            name: self.at.saturating_sub(1),
                        },
                        Span {
                            start,
                            end: operator.span.end,
                        },
                        false,
                    );
                    break;
                }
            } else if self.punct(b'(') || self.punct(b'[') {
                let call = self.punct(b'(');
                let close = if call { b')' } else { b']' };
                self.at += 1;
                let mut arguments = Vec::new();
                let mut valid = true;
                while self.at < self.end && !self.punct(close) {
                    let before = self.at;
                    let label = if call
                        && self.token().is_some_and(|t| t.kind == Kind::Name)
                        && self
                            .doc
                            .tokens
                            .get(self.at + 1)
                            .is_some_and(|t| t.kind == Kind::Punct(b'='))
                    {
                        let i = self.at;
                        self.at += 2;
                        Some(i)
                    } else {
                        None
                    };
                    let value = self.parse(0, depth + 1);
                    valid &= value.is_some_and(|id| self.tree.get(id).complete);
                    arguments.push(Argument { label, value });
                    if self.punct(b',') {
                        self.at += 1;
                    } else {
                        break;
                    }
                    if self.at == before {
                        break;
                    }
                }
                let closed = self.punct(close);
                let end = if closed {
                    let end = self.doc.tokens[self.at].span.end;
                    self.at += 1;
                    end
                } else {
                    self.doc.tokens[self.at.saturating_sub(1)].span.end
                };
                let complete = closed
                    && valid
                    && (call || arguments.len() == 1)
                    && self.tree.get(left).complete;
                left = self.push(
                    if call {
                        ExprKind::Call {
                            callee: left,
                            arguments,
                        }
                    } else {
                        ExprKind::Index { receiver: left }
                    },
                    Span { start, end },
                    complete,
                );
            } else {
                let priority = match operator.kind {
                    Kind::Punct(b'=') => 3,
                    Kind::Operator if self.doc.source.slice(operator.span) == "**" => 6,
                    Kind::Punct(b'<' | b'>') | Kind::Operator => 3,
                    Kind::Punct(b'+' | b'-') => 4,
                    Kind::Punct(b'*' | b'/') => 5,
                    _ => match self.name(operator) {
                        "or" => 1,
                        "and" => 2,
                        "like" | "is" => 3,
                        _ => 0,
                    },
                };
                if priority == 0 || priority < minimum {
                    break;
                }
                self.at += 1;
                let Some(right) = self.parse(
                    if priority == 6 {
                        priority
                    } else {
                        priority + 1
                    },
                    depth + 1,
                ) else {
                    self.tree.nodes[left.0].complete = false;
                    break;
                };
                left = self.push(
                    ExprKind::Binary { left, right },
                    Span {
                        start,
                        end: self.tree.get(right).span.end,
                    },
                    self.tree.get(left).complete && self.tree.get(right).complete,
                );
            }
        }
        Some(left)
    }
}
pub fn parse(doc: &Document, start: usize, end: usize) -> Expressions {
    let mut parser = Parser {
        doc,
        at: start,
        end: end.min(doc.tokens.len()),
        tree: Expressions::default(),
    };
    parser.tree.root = parser.parse(0, 0);
    parser.tree.next = parser.at;
    parser.tree
}
pub fn before(doc: &Document, end: usize) -> Expressions {
    fn start(doc: &Document, end: usize, depth: u16) -> Option<usize> {
        if depth > 128 {
            return None;
        }
        let token = *doc.tokens.get(end)?;
        let mut begin = end;
        if matches!(token.kind, Kind::Punct(b')' | b']')) {
            let close = token.kind;
            let open = if close == Kind::Punct(b')') {
                Kind::Punct(b'(')
            } else {
                Kind::Punct(b'[')
            };
            let mut balance = 1;
            while begin > 0 {
                begin -= 1;
                if doc.tokens[begin].kind == close {
                    balance += 1;
                }
                if doc.tokens[begin].kind == open {
                    balance -= 1;
                }
                if balance == 0 {
                    break;
                }
            }
            if balance != 0 {
                return None;
            }
            if begin > 0 {
                let previous = doc.tokens[begin - 1];
                if close == Kind::Punct(b']')
                    || matches!(previous.kind, Kind::Punct(b')' | b']'))
                    || previous.kind == Kind::Name && !is_keyword(doc.names.get(previous.name))
                {
                    begin = start(doc, begin - 1, depth + 1)?;
                }
            }
        } else if token.kind != Kind::Name {
            return None;
        }
        if begin > 1 && matches!(doc.tokens[begin - 1].kind, Kind::Punct(b'.' | b'!')) {
            begin = start(doc, begin - 2, depth + 1)?;
        }
        Some(begin)
    }
    start(doc, end, 0).map_or_else(Expressions::default, |start| parse(doc, start, end + 1))
}
