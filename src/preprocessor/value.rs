//! Bounded expression evaluation; macro source is never executed as host code.
use crate::syntax::lexer::{self, Kind, Names, Token};
use std::collections::HashMap;
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(i64),
    Text(String),
    Boolean(bool),
}
impl Value {
    pub fn truth(&self) -> bool {
        match self {
            Self::Number(n) => *n != 0,
            Self::Text(s) => !s.is_empty(),
            Self::Boolean(b) => *b,
        }
    }
    pub fn render(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Text(s) => s.clone(),
            Self::Boolean(b) => b.to_string(),
        }
    }
    fn number(&self) -> Option<i64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Boolean(b) => Some(i64::from(*b)),
            Self::Text(s) => s.parse().ok(),
        }
    }
}
pub type Variables = HashMap<String, Option<Value>>;
struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    at: usize,
    variables: &'a Variables,
}
impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.tokens
            .get(self.at)
            .map(|t| &self.source[t.span.start as usize..t.span.end as usize])
    }
    fn parse(&mut self, minimum: u8, depth: u8) -> Option<Value> {
        if depth > 32 {
            return None;
        }
        let token = *self.tokens.get(self.at)?;
        let text = self.peek()?.to_ascii_lowercase();
        self.at += 1;
        let mut left = match text.as_str() {
            "(" => {
                let value = self.parse(0, depth + 1)?;
                if self.peek()? != ")" {
                    return None;
                }
                self.at += 1;
                value
            }
            "$" => {
                let name = self.peek()?.to_ascii_lowercase();
                self.at += 1;
                self.variables.get(&format!("${name}"))?.clone()?
            }
            "not" => Value::Boolean(!self.parse(6, depth + 1)?.truth()),
            "-" => Value::Number(self.parse(6, depth + 1)?.number()?.checked_neg()?),
            "+" => Value::Number(self.parse(6, depth + 1)?.number()?),
            "true" => Value::Boolean(true),
            "false" => Value::Boolean(false),
            _ if token.kind == Kind::String => {
                let s = &self.source[token.span.start as usize..token.span.end as usize];
                Value::Text(s[1..s.len() - 1].replace("''", "'"))
            }
            _ => Value::Number(text.parse().ok()?),
        };
        while let Some(operator) = self.peek() {
            let operator = operator.to_ascii_lowercase();
            let priority = match operator.as_str() {
                "or" => 1,
                "and" => 2,
                "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=" => 3,
                "+" | "-" => 4,
                "*" | "/" => 5,
                _ => 0,
            };
            if priority == 0 || priority < minimum {
                break;
            }
            self.at += 1;
            let right = self.parse(priority + 1, depth + 1)?;
            left = match operator.as_str() {
                "or" => Value::Boolean(left.truth() || right.truth()),
                "and" => Value::Boolean(left.truth() && right.truth()),
                "=" => Value::Boolean(left == right),
                "!=" | "<>" => Value::Boolean(left != right),
                "+" if matches!(left, Value::Text(_)) || matches!(right, Value::Text(_)) => {
                    Value::Text(left.render() + &right.render())
                }
                "+" => Value::Number(left.number()?.checked_add(right.number()?)?),
                "-" => Value::Number(left.number()?.checked_sub(right.number()?)?),
                "*" => Value::Number(left.number()?.checked_mul(right.number()?)?),
                "/" => Value::Number(left.number()?.checked_div(right.number()?)?),
                op => {
                    let comparison = match (&left, &right) {
                        (Value::Text(a), Value::Text(b)) => a.cmp(b),
                        _ => left.number()?.cmp(&right.number()?),
                    };
                    Value::Boolean(match op {
                        "<" => comparison.is_lt(),
                        ">" => comparison.is_gt(),
                        "<=" => !comparison.is_gt(),
                        ">=" => !comparison.is_lt(),
                        _ => return None,
                    })
                }
            };
        }
        Some(left)
    }
}
pub fn evaluate(source: &str, variables: &Variables) -> Option<Value> {
    let lexed = lexer::lex(source, 0, &mut Names::default());
    if !lexed.errors.is_empty() {
        return None;
    }
    let mut parser = Parser {
        source,
        tokens: lexed.tokens,
        at: 0,
        variables,
    };
    let value = parser.parse(0, 0)?;
    (parser.at == parser.tokens.len()).then_some(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_unknowns_and_overflow_are_explicit() {
        let vars = Variables::from([("$mode".into(), Some(Value::Number(2)))]);
        assert_eq!(
            evaluate("$mode * (3 + 1) = 8 and not false", &vars),
            Some(Value::Boolean(true))
        );
        assert_eq!(
            evaluate("'hello ' + 'world'", &vars),
            Some(Value::Text("hello world".into()))
        );
        for source in [
            "$missing",
            "1 / 0",
            "9223372036854775807 + 1",
            "1 rubbish",
            "execute('x')",
        ] {
            assert!(evaluate(source, &vars).is_none());
        }
    }
}
