//! Immutable factual API catalogue, with source and syntax materialized per class.
use super::index::{Binding, DocumentId, Entry};
use crate::{
    source::Source,
    syntax::{self, model::SymbolId},
};
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

const FIRST_BUILTIN: usize = usize::MAX / 2;
const REFERENCE: &str = "https://docs.actian.com/openroad/12.0/SysRefSum/";

#[derive(Deserialize)]
struct Parameter {
    name: String,
    #[serde(rename = "type")]
    ty: Option<String>,
    optional: Option<bool>,
}
#[derive(Deserialize)]
struct Member {
    #[serde(rename = "signatureComplete")]
    signature_complete: Option<bool>,
    origin: String,
    page: String,
    name: String,
    kind: String,
    #[serde(rename = "type")]
    ty: Option<String>,
    #[serde(default)]
    parameters: Vec<Parameter>,
}
#[derive(Deserialize)]
struct Class {
    name: String,
    page: String,
    members: Vec<Member>,
    #[serde(default)]
    ancestors: Vec<String>,
    #[serde(skip)]
    entry: OnceLock<Entry>,
}
struct Catalogue {
    classes: Vec<Class>,
    names: HashMap<String, usize>,
}
fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let classes: Vec<Class> =
            serde_json::from_str(include_str!("../../catalogue/system-classes.json"))
                .expect("the embedded catalogue is validated by tests");
        let names = classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.to_ascii_lowercase(), i))
            .collect();
        Catalogue { classes, names }
    })
}
pub fn binding(name: &str) -> Option<Binding> {
    catalogue()
        .names
        .get(&name.to_ascii_lowercase())
        .map(|&i| Binding {
            document: DocumentId(FIRST_BUILTIN + i),
            symbol: SymbolId(0),
        })
}
pub fn entry(id: DocumentId) -> Option<&'static Entry> {
    let i = id.0.checked_sub(FIRST_BUILTIN)?;
    let class = catalogue().classes.get(i)?;
    Some(class.entry.get_or_init(|| {
        let uri = format!("gorak-builtin:/{}.w4gl", class.name);
        let source = Source::new(uri, class.source()).expect("catalogue source fits byte spans");
        Entry::new(syntax::parse(source), 0)
    }))
}
impl Class {
    fn source(&self) -> String {
        let mut lines = vec![
            "# Read-only OpenROAD 12.0 API reference; generated from documented declarations."
                .into(),
            format!("# {REFERENCE}{}", self.page),
            "[classsource]".into(),
            "[attributes]".into(),
        ];
        for member in self.members.iter().filter(|m| m.kind == "attribute") {
            lines.push(format!(
                "{} = {}",
                member.name,
                serde_json::to_string(member.ty.as_deref().unwrap_or("unknown")).unwrap()
            ));
        }
        lines.push("[methods]".into());
        for method in self.members.iter().filter(|m| m.kind == "method") {
            let signature = method
                .ty
                .as_ref()
                .map_or("METHOD".into(), |ty| format!("METHOD RETURNING {ty}"));
            lines.push(format!(
                "{} = {}",
                method.name,
                serde_json::to_string(&signature).unwrap()
            ));
        }
        lines.push("===".into());
        for method in self.members.iter().filter(|m| m.kind == "method") {
            let parameters = method
                .parameters
                .iter()
                .map(|p| format!("{} = {}", p.name, p.ty.as_deref().unwrap_or("unknown")))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("METHOD {}({parameters}) = {{}}", method.name));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_public_class_generates_valid_source() {
        assert_eq!(catalogue().classes.len(), 156);
        for i in 0..catalogue().classes.len() {
            let entry = entry(DocumentId(FIRST_BUILTIN + i)).unwrap();
            assert!(
                entry.document.errors.is_empty(),
                "{}: {:?}",
                entry.document.component,
                entry.document.errors
            );
        }
    }
}

#[derive(Deserialize)]
pub struct Constant {
    pub name: String,
    pub value: Option<String>,
    page: String,
}
pub fn constants() -> &'static [Constant] {
    static CONSTANTS: OnceLock<Vec<Constant>> = OnceLock::new();
    CONSTANTS.get_or_init(|| {
        serde_json::from_str(include_str!("../../catalogue/constants.json"))
            .expect("embedded constants are validated by tests")
    })
}
pub fn constant(name: &str) -> Option<&'static Constant> {
    static INDEX: OnceLock<HashMap<String, usize>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            constants()
                .iter()
                .enumerate()
                .map(|(i, c)| (c.name.to_ascii_lowercase(), i))
                .collect()
        })
        .get(&name.to_ascii_lowercase())
        .map(|&i| &constants()[i])
}
impl Constant {
    pub fn url(&self) -> String {
        format!(
            "https://docs.actian.com/openroad/12.0/LangRef/{}",
            self.page
        )
    }
}
pub fn class_names() -> impl Iterator<Item = &'static str> {
    catalogue().classes.iter().map(|c| c.name.as_str())
}
pub fn documentation(binding: Binding) -> Option<String> {
    let class = catalogue()
        .classes
        .get(binding.document.0.checked_sub(FIRST_BUILTIN)?)?;
    let document = &entry(binding.document)?.document;
    let symbol = document.symbol(binding.symbol);
    let member = class
        .members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(&symbol.spelling));
    Some(format!(
        "{REFERENCE}{}",
        member.map_or(&class.page, |m| &m.page)
    ))
}
/// Inherited API entries have the same identity in every class reference view.
pub fn canonical(binding: Binding) -> Option<Binding> {
    let class = catalogue()
        .classes
        .get(binding.document.0.checked_sub(FIRST_BUILTIN)?)?;
    let doc = &entry(binding.document)?.document;
    let symbol = doc.symbol(binding.symbol);
    let owner = symbol.owner.map(|id| doc.symbol(id)).unwrap_or(symbol);
    let member = class
        .members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(&owner.spelling))?;
    let origin = self::binding(&member.origin)?;
    if origin.document == binding.document {
        return None;
    }
    let origin_doc = &entry(origin.document)?.document;
    let (i, original) = origin_doc
        .symbols
        .iter()
        .enumerate()
        .find(|(_, s)| s.scope == 0 && s.spelling.eq_ignore_ascii_case(&owner.spelling))?;
    let id = if symbol.parameter {
        let implementation = origin_doc
            .symbols
            .iter()
            .find(|s| s.implementation && s.canonical == original.canonical)?;
        *implementation.parameters.iter().find(|&&p| {
            origin_doc
                .symbol(p)
                .spelling
                .eq_ignore_ascii_case(&symbol.spelling)
        })?
    } else {
        origin_doc.canonical(SymbolId(i as u32))
    };
    Some(Binding {
        document: origin.document,
        symbol: id,
    })
}
pub fn ancestors(id: DocumentId) -> Option<&'static [String]> {
    Some(
        &catalogue()
            .classes
            .get(id.0.checked_sub(FIRST_BUILTIN)?)?
            .ancestors,
    )
}
pub fn signature_complete(binding: Binding) -> bool {
    let Some(class) = binding
        .document
        .0
        .checked_sub(FIRST_BUILTIN)
        .and_then(|i| catalogue().classes.get(i))
    else {
        return true;
    };
    let doc = &entry(binding.document).unwrap().document;
    let name = &doc.symbol(binding.symbol).spelling;
    class
        .members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .is_some_and(|m| m.signature_complete == Some(true))
}
pub fn parameter_optional(owner: Binding, name: &str) -> bool {
    let Some(class) = owner
        .document
        .0
        .checked_sub(FIRST_BUILTIN)
        .and_then(|i| catalogue().classes.get(i))
    else {
        return true;
    };
    let doc = &entry(owner.document).unwrap().document;
    class
        .members
        .iter()
        .find(|m| {
            m.name
                .eq_ignore_ascii_case(&doc.symbol(owner.symbol).spelling)
        })
        .and_then(|m| {
            m.parameters
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
        })
        .is_none_or(|p| p.optional != Some(false))
}
pub const CONTEXTS: &[(&str, &str)] = &[
    ("CurFrame", "FrameExec"),
    ("CurProcedure", "ProcExec"),
    ("CurMethod", "MethodExec"),
    ("CurObject", "UserObject"),
    (
        "CurExec",
        "FrameExec / ProcExec / MethodExec (context-dependent)",
    ),
    ("CurSession", "SessionObject"),
    ("IIDBMSerror", "INTEGER"),
    ("IIerrornumber", "INTEGER"),
    ("IIrowcount", "INTEGER"),
    ("CurEventScope", "Scope"),
    ("CurScriptScope", "Scope"),
];
