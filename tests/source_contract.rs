//! Independently authored consumer cases for source contract 12.
use gorak_lsp_rs::{
    analysis::Engine,
    source::{Position, Range, Source},
};

fn at(text: &str, needle: &str) -> Position {
    Source::new("file:///fixture", text)
        .unwrap()
        .position(text.find(needle).unwrap() as u32)
}

#[test]
fn structured_members_keep_types_references_and_versioned_rename_in_original_source() {
    let uri = "file:///contract/app/container.w4gl";
    let text = r#"[classsource]
superclass = "userobject"
[attributes]
"slot$value" = { remark = "😀 library!record", declaration = "PRIVATE library!record DEFAULT NULL", taggedvalues = [{}, {name = "slot$value", value = "  "}] }
rows = "PRIVATE ARRAY OF library!record DEFAULT NULL"
caption = "PRIVATE VARCHAR(40) NOT NULL DEFAULT '  '"
[methods]
obtainRecord = { remark = "😀 obtainRecord", declaration = "PRIVATE METHOD RETURNING library!record", taggedvalues = [{}, {}] }
===
METHOD obtainRecord() = { RETURN self.slot$value; }
METHOD inspect() = { self.obtainRecord(); }
"#.replace('\n', "\r\n");
    let mut engine = Engine::default();
    engine.graph.set(
        "/contract/app".into(),
        r#"{"appflags":"  7  ","included_applications":["library"]}"#,
    );
    let class = "file:///contract/library/record.w4gl";
    engine.update(class, "[classsource]\n", 1).unwrap();
    engine.update(uri, &text, 17).unwrap();
    assert_eq!(engine.diagnostics(uri), serde_json::json!([]));
    let doc = gorak_lsp_rs::syntax::parse(Source::new(uri, text.as_str()).unwrap());
    assert!(doc.errors.is_empty(), "{:?}", doc.errors);
    for name in ["slot$value", "rows", "caption", "obtainRecord"] {
        let symbol = doc.symbols.iter().find(|s| s.spelling == name).unwrap();
        assert_eq!(doc.source.slice(symbol.span), name);
        assert!(symbol.ty.is_some(), "{name}");
    }
    for name in ["remark", "declaration", "taggedvalues", "name", "value"] {
        assert!(!doc.symbols.iter().any(|s| s.spelling == name));
    }
    let definitions = engine.definitions(uri, at(&text, "record DEFAULT"));
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].uri.as_ref(), class);
    assert!(
        engine
            .definitions(uri, at(&text, "library!record\","))
            .is_empty()
    );
    let references = engine.references(
        class,
        Position {
            line: 0,
            character: 2,
        },
        false,
        false,
    );
    assert_eq!(references.len(), 3);
    let source = Source::new(uri, text.as_str()).unwrap();
    for reference in references {
        let begin = source.offset(reference.range.start) as usize;
        let end = source.offset(reference.range.end) as usize;
        assert_eq!(&text[begin..end], "record");
    }
    let outline = engine.document_symbols(uri).to_string();
    assert!(outline.contains("inspect") && outline.contains("obtainRecord"));
    let completions = engine.completions(uri, at(&text, "obtainRecord();"));
    assert!(
        completions
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["label"] == "obtainRecord")
    );
    let edits = engine
        .rename(uri, at(&text, "obtainRecord();"), "retrieve")
        .unwrap();
    let changes = edits["documentChanges"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["textDocument"]["version"], 17);
    let edits = changes[0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 3);
    for edit in edits {
        let range: Range = serde_json::from_value(edit["range"].clone()).unwrap();
        let start = source.offset(range.start) as usize;
        let end = source.offset(range.end) as usize;
        assert_eq!(&text[start..end], "obtainRecord");
        assert_ne!(range.start, at(&text, "obtainRecord\","));
    }
    assert!(
        engine
            .rename(uri, at(&text, "slot$value;"), "other")
            .is_err()
    );
    let dynamic = text.replace(
        "self.obtainRecord();",
        "self.obtainRecord(); self.:method_name();",
    );
    engine.update(uri, &dynamic, 18).unwrap();
    assert!(
        engine
            .rename(uri, at(&dynamic, "obtainRecord();"), "retrieve")
            .is_err()
    );
}

#[test]
fn implicit_core_precedes_direct_includes_without_leaking_transitive_sources() {
    let mut engine = Engine::default();
    let uri = "file:///contract/app/main.w4gl";
    let text =
        "[proc4glsource]\n===\nPROCEDURE main() = DECLARE item = shared_type; { MESSAGE item; }";
    let core = "file:///contract/core/shared_type.w4gl";
    let other = "file:///contract/library/shared_type.w4gl";
    engine.graph.set(
        "/contract/app".into(),
        r#"{"appflags":"","included_applications":["library"]}"#,
    );
    engine.graph.set(
        "/contract/library".into(),
        r#"{"included_applications":["hidden"]}"#,
    );
    engine.update(core, "[classsource]\n", 1).unwrap();
    engine.update(other, "[classsource]\n", 1).unwrap();
    engine
        .update("file:///contract/hidden/secret.w4gl", "[classsource]\n", 1)
        .unwrap();
    engine.update(uri, text, 1).unwrap();
    assert_eq!(
        engine.definitions(uri, at(text, "shared_type;"))[0]
            .uri
            .as_ref(),
        core
    );
    let qualified = text.replace("shared_type", "core!shared_type");
    engine.update(uri, &qualified, 2).unwrap();
    assert_eq!(
        engine.definitions(uri, at(&qualified, "shared_type;"))[0]
            .uri
            .as_ref(),
        core
    );
    engine.graph.set(
        "/contract/app".into(),
        r#"{"included_applications":["library","core"]}"#,
    );
    engine.update(uri, text, 3).unwrap();
    assert_eq!(
        engine.definitions(uri, at(text, "shared_type;"))[0]
            .uri
            .as_ref(),
        core
    );
    let hidden = text.replace("shared_type", "secret");
    engine.update(uri, &hidden, 4).unwrap();
    assert!(engine.definitions(uri, at(&hidden, "secret;")).is_empty());
    engine.graph.set(
        "/contract/app".into(),
        r#"{"included_applications":["library",{"name":"core","image":"runtime.img"}]}"#,
    );
    engine.update(uri, text, 5).unwrap();
    assert!(engine.definitions(uri, at(text, "shared_type;")).is_empty());
}

#[test]
fn literal_defaults_and_ordered_empty_wml_rows_are_not_language_declarations() {
    let mut engine = Engine::default();
    let uri = "file:///contract/app/constant.w4gl";
    for (metadata, expected) in [
        ("", None),
        ("defaultstring = \"\"", Some("''")),
        ("defaultstring = \"  \"", Some("'  '")),
        (
            "defaultstring = \" leading trailing \"",
            Some("' leading trailing '"),
        ),
    ] {
        let text = format!("[constsource]\ndatatype = \"varchar(40)\"\n{metadata}\n");
        let doc = gorak_lsp_rs::syntax::parse(Source::new(uri, text).unwrap());
        assert_eq!(doc.symbols[0].constant_value.as_deref(), expected);
    }
    let uri = "file:///contract/app/view.wml";
    let text = r#"<frame><topform width="0" height="0">
<entryfield name="balance" datatype="integer" width="0" height="0" fieldstyle="2"><defaultstring>  </defaultstring><taggedvalues><row/><row name="opaque" value="  "/><row/></taggedvalues></entryfield>
<boxshape width="0" height="0"/>
<script><![CDATA[INITIALIZE = { MESSAGE '😀'; balance = 2; }]]></script>
</topform></frame>"#.replace('\n', "\r\n");
    engine.update(uri, &text, 3).unwrap();
    assert_eq!(engine.diagnostics(uri), serde_json::json!([]));
    let definitions = engine.definitions(uri, at(&text, "balance ="));
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].range.start, at(&text, "balance\""));
    assert!(!engine.document_symbols(uri).to_string().contains("opaque"));
    let directory = tempfile::tempdir().unwrap();
    for name in ["field_defaults.json", "view.fielddefaults.json"] {
        let path = directory.path().join(name);
        std::fs::write(&path, r#"{"absent":true,"groups":{"entryfield":{"styles":{"style2":{"datatype":"varchar"}}}}}"#).unwrap();
        assert!(gorak_lsp_rs::workspace::load(&path).is_none());
    }
}

#[test]
fn metadata_spans_skip_comments_and_defaults_and_reject_uneditable_identifiers() {
    let uri = "file:///contract/app/holder.w4gl";
    let text = r#"# classsource in an opaque comment
[classsource]
[attributes]
item = { remark = "😀 ate", declaration = "PRIVATE ate DEFAULT '\n' " }
[methods.perform]
remark = """Metadata only:
pretend = "METHOD RETURNING ate"
"""
declaration = "PRIVATE METHOD RETURNING ate"
===
METHOD perform() = { RETURN self.item; }
METHOD caller() = { self.perform(); }
"#;
    let mut engine = Engine::default();
    let class = "file:///contract/app/ate.w4gl";
    engine.update(class, "[classsource]\n", 1).unwrap();
    engine.update(uri, text, 5).unwrap();
    assert_eq!(engine.diagnostics(uri), serde_json::json!([]));
    assert_eq!(
        engine.definitions(uri, at(text, "ate DEFAULT"))[0]
            .uri
            .as_ref(),
        class
    );
    let doc = gorak_lsp_rs::syntax::parse(Source::new(uri, text).unwrap());
    assert_eq!(
        doc.source.range(doc.symbols[0].span).start,
        Position {
            line: 1,
            character: 1
        }
    );
    assert!(!doc.symbols.iter().any(|s| s.spelling == "pretend"));
    assert_eq!(
        engine
            .rename(uri, at(text, "perform();"), "performAgain")
            .unwrap()["documentChanges"][0]["edits"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let escaped = text.replace("[methods.perform]", "[methods.\"per\\u0066orm\"]");
    engine.update(uri, &escaped, 6).unwrap();
    assert!(
        engine
            .diagnostics(uri)
            .to_string()
            .contains("metadata-identifier")
    );
    assert!(
        engine
            .rename(uri, at(&escaped, "perform();"), "performAgain")
            .is_err()
    );
}

#[test]
fn ambiguous_structured_methods_still_refuse_rename() {
    let uri = "file:///contract/app/object.w4gl";
    let text = "[classsource]\n[methods]\nact = { declaration = \"METHOD RETURNING INTEGER\", remark = \"  \" }\n===\nMETHOD act() = { RETURN 1; }\nMETHOD act() = { RETURN 2; }\nMETHOD caller() = { self.act(); }";
    let mut engine = Engine::default();
    engine.update(uri, text, 1).unwrap();
    assert!(
        engine
            .diagnostics(uri)
            .to_string()
            .contains("duplicate-declaration")
    );
    assert!(engine.rename(uri, at(text, "act();"), "doWork").is_err());
}

#[test]
fn core_ambiguity_and_own_application_precedence_remain_explicit() {
    use gorak_lsp_rs::project::{Graph, LookupIssue};
    let mut graph = Graph::default();
    graph.ensure("/contract/app".into());
    graph.ensure("/contract/core".into());
    graph.ensure("/contract/CORE".into());
    let lookup = graph.lookup("/contract/app", None, |_| Vec::<u32>::new());
    assert_eq!(lookup.issues, vec![LookupIssue::AmbiguousApplication]);
    let own = graph.lookup("/contract/app", None, |app| {
        if app == "/contract/app" {
            vec![1]
        } else {
            vec![]
        }
    });
    assert_eq!(own.candidates, vec![1]);
    assert!(own.issues.is_empty());
}
