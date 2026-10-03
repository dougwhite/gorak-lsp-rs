//! Behavioural checks against the pinned canonical gorak project.
use gorak_lsp_rs::{
    analysis::Engine,
    source::{Position, Source},
};
use std::{fs, path::PathBuf};

fn project() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pin: toml::Value = fs::read_to_string(root.join("ecosystem.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let checkout = root.join(".ci/gorak");
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Git is required to verify the fixture pin");
    assert!(head.status.success());
    let tag = format!(
        "refs/tags/{}^{{commit}}",
        pin["gorak_revision"].as_str().unwrap()
    );
    let pinned = std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["rev-parse", "--verify", &tag])
        .output()
        .expect("Git is required to resolve the fixture tag");
    assert!(
        pinned.status.success(),
        "Run python scripts/fetch-compatibility.py before cargo test"
    );
    assert_eq!(
        head.stdout, pinned.stdout,
        "Fixture checkout must match the pinned tag"
    );
    let upstream: toml::Value = fs::read_to_string(checkout.join("ecosystem.toml"))
        .expect("Run python scripts/fetch-compatibility.py before cargo test")
        .parse()
        .unwrap();
    assert_eq!(pin["source_version"], upstream["source_version"]);
    checkout.join("compatibility/project")
}

fn position(text: &str, needle: &str) -> Position {
    Source::new("file:///fixture", text)
        .unwrap()
        .position(text.find(needle).expect("fixture marker exists") as u32)
}

fn load() -> (Engine, PathBuf) {
    let root = project();
    let mut engine = Engine::default();
    for entry in walkdir::WalkDir::new(&root) {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(path).unwrap();
        if path.file_name().unwrap() == "app.json" {
            engine
                .graph
                .set(path.parent().unwrap().to_string_lossy().into_owned(), &text);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("w4gl" | "wml")
        ) {
            let uri = url::Url::from_file_path(path).unwrap();
            engine.update(uri.as_str(), &text, 1).unwrap();
        }
    }
    (engine, root)
}

#[test]
fn included_class_and_embedded_procedure_calls_resolve() {
    let (engine, root) = load();
    let panel = root.join("example/panel.w4gl");
    let text = fs::read_to_string(&panel).unwrap();
    let uri = url::Url::from_file_path(panel).unwrap();
    let definitions = engine.definitions(uri.as_str(), position(&text, "counter;"));
    assert_eq!(definitions.len(), 1);
    let expected = url::Url::from_file_path(root.join("shared/counter.w4gl")).unwrap();
    assert_eq!(
        definitions[0].uri.as_ref(),
        gorak_lsp_rs::source::canonical_uri(expected.as_str()).as_str()
    );

    let markup = root.join("example/panel.wml");
    let text = fs::read_to_string(&markup).unwrap();
    let uri = url::Url::from_file_path(markup).unwrap();
    let definitions = engine.definitions(uri.as_str(), position(&text, "score(capsules"));
    assert_eq!(definitions.len(), 1);
    let expected = url::Url::from_file_path(root.join("example/score.w4gl")).unwrap();
    assert_eq!(
        definitions[0].uri.as_ref(),
        gorak_lsp_rs::source::canonical_uri(expected.as_str()).as_str()
    );
    let source = fs::read_to_string(root.join("example/score.w4gl")).unwrap();
    assert_eq!(definitions[0].range.start, position(&source, "score("));
}

#[test]
fn explicit_fields_and_character_instructions_preserve_source_locations() {
    let (engine, root) = load();
    let text = fs::read_to_string(root.join("example/panel.wml")).unwrap();
    let uri = url::Url::from_file_path(root.join("example/panel.wml")).unwrap();
    let doc = gorak_lsp_rs::syntax::parse(Source::new(uri.as_str(), text.as_str()).unwrap());
    assert!(doc.errors.is_empty(), "{:?}", doc.errors);
    for name in ["quantity", "amount"] {
        let field = doc.symbols.iter().find(|s| s.spelling == name).unwrap();
        assert!(
            field
                .ty
                .as_ref()
                .unwrap()
                .display
                .eq_ignore_ascii_case("INTEGER")
        );
        assert_eq!(doc.source.slice(field.span), name);
    }
    assert!(
        !doc.tokens
            .iter()
            .any(|t| doc.source.slice(t.span) == "ingres_invalidxmlchar")
    );
    let definitions = engine.definitions(uri.as_str(), position(&text, "quantity = CALLPROC"));
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].range.start, position(&text, "quantity\""));
    let outline = engine.document_symbols(uri.as_str()).to_string();
    assert!(outline.contains("calculate"));
    assert!(outline.contains("click"));
}
