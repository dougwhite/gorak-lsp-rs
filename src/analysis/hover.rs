use super::{
    builtins,
    index::{Engine, application},
};
use crate::{source::Position, syntax::lexer::Kind};
use serde_json::{Value, json};
use std::path::Path;

impl Engine {
    pub fn hover(&self, uri: &str, position: Position) -> Value {
        let Some(id) = self.id(uri) else {
            return Value::Null;
        };
        let doc = self.document(id);
        let token = doc.token_at(self.query_offset(id, position));
        let targets = self.target(uri, position);
        if let Some(i) = token {
            let name = doc.names.get(doc.tokens[i].name);
            if let Some(&(spelling, datatype)) = builtins::CONTEXTS
                .iter()
                .find(|(word, _)| word.eq_ignore_ascii_case(name))
            {
                let unqualified =
                    i == 0 || !matches!(doc.tokens[i - 1].kind, Kind::Punct(b'.' | b'!'));
                if unqualified {
                    return json!({
                        "contents": {"kind":"markdown", "value":format!("**{spelling}**: {datatype}\n\nOpenROAD context variable")},
                        "range":self.span_location(id, doc.tokens[i].span).range
                    });
                }
            }
        }
        if targets.is_empty() {
            if let Some(i) = token
                .filter(|&i| i == 0 || !matches!(doc.tokens[i - 1].kind, Kind::Punct(b'.' | b'!')))
                && let Some(constant) = builtins::constant(doc.names.get(doc.tokens[i].name))
            {
                return json!({"contents":{"kind":"markdown","value":format!("**{}**{}\n\nOpenROAD system constant\n\n[OpenROAD 12.0 reference]({})",constant.name,constant.value.as_ref().map_or(String::new(),|v|format!(" = {v}")),constant.url())},"range":self.span_location(id,doc.tokens[i].span).range});
            }
            return Value::Null;
        }
        if targets.len() != 1 {
            return Value::Null;
        }
        let binding = targets[0];
        let symbol = self.symbol(binding);
        let range = token.map(|i| self.span_location(id, doc.tokens[i].span).range);
        let contents = if let Some(url) = builtins::documentation(binding) {
            json!({"kind":"markdown","value":format!("**{}**{}\n\nOpenROAD system API\n\n[Language reference]({url})",symbol.spelling,symbol.ty.as_ref().map_or(String::new(),|t|format!(": {}",t.display)))})
        } else {
            let owner = self.document(binding.document);
            let app = application(&owner.source.uri);
            let name = Path::new(&app)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            json!({"kind":"plaintext","value":format!("{}{}{}\n{} · {}{}",symbol.spelling,symbol.ty.as_ref().map_or(String::new(),|t|format!(": {}",t.display)),symbol.constant_value.as_ref().map_or(String::new(), |v| format!(" = {v}")),owner.component,name,if symbol.local {"\nRoutine-local variable"}else{""})})
        };
        let mut value = json!({"contents":contents});
        if let Some(range) = range {
            value["range"] = json!(range);
        }
        value
    }
}
