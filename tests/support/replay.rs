//! Replay synthetic behavioural oracle cases without an editor or server process.
use gorak_lsp_rs::{
    analysis::Engine,
    project::{Application, Include},
    source::Position,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    input: Input,
}
#[derive(Deserialize)]
struct Input {
    files: Vec<File>,
    applications: Vec<App>,
    method: String,
    args: Vec<Value>,
}
#[derive(Deserialize)]
struct File {
    uri: String,
    text: String,
    #[serde(default)]
    version: i32,
}
#[derive(Deserialize)]
struct App {
    directory: String,
    includes: Vec<Include>,
    problems: Vec<Value>,
}

fn replay(input: Input) -> anyhow::Result<Value> {
    let mut engine = Engine::default();
    for file in input.files {
        engine
            .update(&file.uri, &file.text, file.version)
            .map_err(anyhow::Error::msg)?;
    }
    for app in input.applications {
        engine.graph.insert(Application {
            directory: app.directory,
            includes: app.includes,
            valid: app.problems.is_empty(),
        });
    }
    engine.prepare();
    let uri = input
        .args
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default();
    let position = input
        .args
        .get(1)
        .cloned()
        .map(serde_json::from_value::<Position>)
        .transpose()?
        .unwrap_or_default();
    let result = match input.method.as_str() {
        "definitions" => json!(engine.definitions(uri, position)),
        "references" => json!(engine.references(
            uri,
            position,
            input.args.get(2).and_then(Value::as_bool).unwrap_or(true),
            false
        )),
        "highlights" => json!(engine.references(uri, position, true, true)),
        "hover" => engine.hover(uri, position),
        "signatureHelp" => engine.signature_help(uri, position),
        "documentSymbols" => engine.document_symbols(uri),
        "completions" => engine.completions_with_snippets(
            uri,
            position,
            input.args.get(2).and_then(Value::as_bool).unwrap_or(false),
        ),
        "workspaceSymbols" => engine.workspace_symbols(uri),
        "diagnostics" => engine.diagnostics(uri),
        "rename" => match engine.rename(
            uri,
            position,
            input
                .args
                .get(2)
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            Ok(value) => value,
            Err(error) => return Ok(json!({"error": error})),
        },
        method => anyhow::bail!("Unknown oracle method: {method}"),
    };
    Ok(json!({"actual": result}))
}
pub fn run_case(case: Value) -> anyhow::Result<Value> {
    let case: Case = serde_json::from_value(case)?;
    replay(case.input)
}
