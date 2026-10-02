//! Expanded syntax retains a reversible map to physical source documents.
mod value;
use crate::source::{Source, Span};
use std::{collections::HashSet, sync::Arc};
use value::{Value, Variables};

pub struct Origin {
    pub expanded: Span,
    pub source: Source,
    pub original: Span,
    generated: bool,
}
#[derive(Default)]
pub struct Expansion {
    pub text: String,
    pub origins: Vec<Origin>,
    pub issues: Vec<String>,
    pub dependencies: HashSet<Arc<str>>,
}
impl Expansion {
    pub fn location(&self, span: Span) -> Option<(&Source, Span)> {
        let i = self
            .origins
            .partition_point(|p| p.expanded.end <= span.start);
        let origin = self.origins.get(i)?;
        if span.start < origin.expanded.start || span.end > origin.expanded.end {
            return None;
        }
        let original = if origin.generated {
            origin.original
        } else {
            Span {
                start: origin.original.start + span.start - origin.expanded.start,
                end: origin.original.start + span.end - origin.expanded.start,
            }
        };
        Some((&origin.source, original))
    }
    pub fn offset(&self, uri: &str, byte: u32) -> Option<u32> {
        self.origins
            .iter()
            .find(|o| {
                o.source.uri.as_ref() == uri && o.original.start <= byte && byte <= o.original.end
            })
            .map(|o| {
                if o.generated {
                    o.expanded.start
                } else {
                    o.expanded.start + byte - o.original.start
                }
            })
    }
    fn append(&mut self, text: &str, source: &Source, span: Span, generated: bool) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len() as u32;
        self.text.push_str(text);
        let end = self.text.len() as u32;
        if !generated
            && let Some(previous) = self.origins.last_mut().filter(|p| {
                !p.generated
                    && p.source.uri == source.uri
                    && p.original.end == span.start
                    && p.expanded.end == start
            })
        {
            previous.expanded.end = end;
            previous.original.end = span.end;
            return;
        }
        self.origins.push(Origin {
            expanded: Span { start, end },
            source: source.clone(),
            original: span,
            generated,
        });
    }
}
struct Branch {
    parent: bool,
    value: Option<bool>,
    alternate: bool,
}
struct Expander<'a, F> {
    result: Expansion,
    variables: Variables,
    lookup: &'a F,
    limit: usize,
}
impl<F: Fn(&str) -> Option<Source>> Expander<'_, F> {
    fn emit(&mut self, source: &Source, start: usize, stack: &mut Vec<Arc<str>>) {
        if stack.len() >= 20 || stack.contains(&source.uri) {
            self.result.issues.push("include-cycle-or-depth".into());
            return;
        }
        stack.push(source.uri.clone());
        let mut branches: Vec<Branch> = Vec::new();
        let mut enabled = true;
        let mut offset = start;
        for line in source.text()[start..].split_inclusive('\n') {
            let span = Span::new(offset, offset + line.len());
            if let Some(directive) = line.strip_prefix('#') {
                let (command, argument) = directive
                    .trim_start()
                    .split_once(char::is_whitespace)
                    .unwrap_or((directive.trim(), ""));
                let argument = argument.split("--").next().unwrap_or_default().trim();
                match command.to_ascii_lowercase().as_str() {
                    "if" | "ifdef" | "ifndef" => {
                        let test = match command.to_ascii_lowercase().as_str() {
                            "ifdef" => {
                                Some(self.variables.contains_key(&argument.to_ascii_lowercase()))
                            }
                            "ifndef" => {
                                Some(!self.variables.contains_key(&argument.to_ascii_lowercase()))
                            }
                            _ => value::evaluate(
                                argument
                                    .strip_suffix("THEN")
                                    .or_else(|| argument.strip_suffix("then"))
                                    .unwrap_or(argument)
                                    .trim(),
                                &self.variables,
                            )
                            .map(|v| v.truth()),
                        };
                        if enabled && test.is_none() {
                            self.result
                                .issues
                                .push("unknown-preprocessor-condition".into());
                        }
                        branches.push(Branch {
                            parent: enabled,
                            value: test,
                            alternate: false,
                        });
                        enabled &= test == Some(true);
                    }
                    "else" => match branches.last_mut() {
                        Some(branch) if !branch.alternate => {
                            branch.alternate = true;
                            enabled = branch.parent && branch.value == Some(false);
                        }
                        _ => self.result.issues.push("invalid-preprocessor-else".into()),
                    },
                    "endif" => match branches.pop() {
                        Some(branch) => enabled = branch.parent,
                        None => self
                            .result
                            .issues
                            .push("unexpected-preprocessor-endif".into()),
                    },
                    "define" if enabled => {
                        let (name, expression) = argument
                            .split_once(char::is_whitespace)
                            .unwrap_or((argument, ""));
                        if !name.starts_with('$') || name.len() < 2 {
                            self.result.issues.push("invalid-macro-definition".into());
                        } else {
                            let value = value::evaluate(expression, &self.variables);
                            if !expression.is_empty() && value.is_none() {
                                self.result.issues.push("unknown-macro-value".into());
                            }
                            self.variables.insert(name.to_ascii_lowercase(), value);
                        }
                    }
                    "undef" if enabled => {
                        self.variables.remove(&argument.to_ascii_lowercase());
                    }
                    "include" if enabled => {
                        let name = self.substitute(argument);
                        if let Some(included) = (self.lookup)(&name) {
                            self.result.dependencies.insert(included.uri.clone());
                            self.emit(&included, body_start(included.text()), stack);
                        } else {
                            self.result.issues.push("unresolved-script-include".into());
                        }
                    }
                    command if enabled && !command.starts_with("--") => self
                        .result
                        .issues
                        .push("unsupported-preprocessor-directive".into()),
                    _ => {}
                }
                self.blank(line, source, span);
            } else if !enabled {
                self.blank(line, source, span);
            } else {
                self.line(line, source, offset);
            }
            offset += line.len();
            if self.result.text.len() > self.limit {
                self.result
                    .issues
                    .push("preprocessor-expansion-budget".into());
                break;
            }
        }
        if !branches.is_empty() {
            self.result
                .issues
                .push("unclosed-preprocessor-condition".into());
        }
        stack.pop();
    }
    fn blank(&mut self, line: &str, source: &Source, span: Span) {
        let blank: String = line
            .bytes()
            .map(|b| {
                if b == b'\r' || b == b'\n' {
                    b as char
                } else {
                    ' '
                }
            })
            .collect();
        self.result.append(&blank, source, span, false);
    }
    fn substitute(&self, text: &str) -> String {
        let mut result = String::new();
        let mut cursor = 0;
        for (start, end) in macros(text) {
            result.push_str(&text[cursor..start]);
            result.push_str(
                &self
                    .variables
                    .get(&text[start..end].to_ascii_lowercase())
                    .and_then(Option::as_ref)
                    .map_or_else(|| text[start..end].into(), Value::render),
            );
            cursor = end;
        }
        result.push_str(&text[cursor..]);
        result
    }
    fn line(&mut self, line: &str, source: &Source, offset: usize) {
        let mut cursor = 0;
        for (start, end) in macros(line) {
            self.result.append(
                &line[cursor..start],
                source,
                Span::new(offset + cursor, offset + start),
                false,
            );
            if let Some(value) = self
                .variables
                .get(&line[start..end].to_ascii_lowercase())
                .and_then(Option::as_ref)
            {
                self.result.append(
                    &value.render(),
                    source,
                    Span::new(offset + start, offset + end),
                    true,
                );
            } else {
                self.result.issues.push("undefined-macro".into());
                self.result.append(
                    &line[start..end],
                    source,
                    Span::new(offset + start, offset + end),
                    false,
                );
            }
            cursor = end;
        }
        self.result.append(
            &line[cursor..],
            source,
            Span::new(offset + cursor, offset + line.len()),
            false,
        );
    }
}
fn macros(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$'
            && b.get(i + 1)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            let start = i;
            i += 2;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            result.push((start, i));
        } else {
            i += 1;
        }
    }
    result
}
pub fn body_start(text: &str) -> usize {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim() == "===" {
            return offset;
        }
    }
    text.len()
}
pub fn expand(
    root: &Source,
    application: &str,
    component: &str,
    lookup: impl Fn(&str) -> Option<Source>,
) -> Expansion {
    let variables = Variables::from([
        (
            "$_applicationname".into(),
            Some(Value::Text(application.into())),
        ),
        (
            "$_componentname".into(),
            Some(Value::Text(component.into())),
        ),
    ]);
    let mut expander = Expander {
        result: Expansion::default(),
        variables,
        lookup: &lookup,
        limit: root.text().len() + 64 * 1024 * 1024,
    };
    let start = body_start(root.text());
    expander
        .result
        .append(&root.text()[..start], root, Span::new(0, start), false);
    expander.emit(root, start, &mut Vec::new());
    expander.result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn included_and_conditional_text_maps_to_physical_utf8() {
        let root=Source::new("file:///synthetic/main.w4gl","[proc4glsource]\n===\n#define $on 1\n#if $on = 1\n#include locals\n#else\nwrong\n#endif\n").unwrap();
        let included = Source::new(
            "file:///synthetic/locals.w4gl",
            "[scriptsource]\n===\ncaption = VARCHAR(40); // 🎈\n",
        )
        .unwrap();
        let expansion = expand(&root, "app", "main", |name| {
            (name == "locals").then(|| included.clone())
        });
        assert!(expansion.issues.is_empty());
        assert!(!expansion.text.contains("wrong"));
        let start = expansion.text.find("caption").unwrap();
        let (source, span) = expansion.location(Span::new(start, start + 7)).unwrap();
        assert_eq!(source.uri, included.uri);
        assert_eq!(source.slice(span), "caption");
    }
}
