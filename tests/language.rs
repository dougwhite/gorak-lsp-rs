use gorak_lsp_rs::{
    analysis::Engine,
    source::{Position, Source},
};

fn position(text: &str, needle: &str) -> Position {
    Source::new("file:///fixture", text)
        .unwrap()
        .position(text.find(needle).expect("test marker exists") as u32)
}
#[test]
fn local_scopes_exclude_comments_literals_and_other_methods() {
    let uri = "file:///synthetic/app/account.w4gl";
    let text = "[classsource]\n===\nMETHOD First() = DECLARE amount = VARCHAR(40); BEGIN amount = 'amount'; MESSAGE amount; END;\nMETHOD Other() = DECLARE amount = INTEGER; { amount = 1; }\n// amount";
    let mut engine = Engine::default();
    engine.update(uri, text, 8).unwrap();
    let at = position(text, "amount = '");
    let refs = engine.references(uri, at, true, false);
    assert_eq!(refs.len(), 3);
    assert!(refs.iter().all(|r| r.range.start.line == 2));
    assert!(engine.hover(uri, at).to_string().contains("VARCHAR(40)"));
    let edit = engine.rename(uri, at, "total").unwrap();
    assert_eq!(
        edit["documentChanges"][0]["edits"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(edit["documentChanges"][0]["textDocument"]["version"], 8);
}
#[test]
fn inherited_members_and_named_arguments_bind_to_the_declaration() {
    let mut engine = Engine::default();
    let base = "file:///synthetic/app/base.w4gl";
    let base_text = "[classsource]\n[methods]\nSave = \"METHOD RETURNING VARCHAR(80)\"\n===\nMETHOD Save(caption = VARCHAR(40)) = { RETURN caption; }";
    engine.update(base, base_text, 1).unwrap();
    engine
        .update(
            "file:///synthetic/app/child.w4gl",
            "[classsource]\nsuperclass = \"base\"\n===",
            0,
        )
        .unwrap();
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE item = child; { item.Save(caption = 'test'); }";
    engine.update(uri, text, 2).unwrap();
    let method = engine.definitions(uri, position(text, "Save("));
    assert_eq!(method.len(), 1);
    assert_eq!(method[0].uri.as_ref(), base);
    assert_eq!(method[0].range.start, position(base_text, "Save(caption"));
    let parameter = engine.definitions(uri, position(text, "caption ="));
    assert_eq!(parameter[0].range.start, position(base_text, "caption ="));
    let signature = engine.signature_help(uri, position(text, "'test'"));
    assert_eq!(signature["activeParameter"], 0);
    assert!(signature.to_string().contains("VARCHAR(40)"));
}
#[test]
fn array_elements_resolve_system_methods_without_copying_catalogue_into_workspace() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE rows = ARRAY OF StringObject; { rows[1].LocateString(); }";
    engine.update(uri, text, 1).unwrap();
    let definitions = engine.definitions(uri, position(text, "LocateString"));
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0].uri.as_ref(),
        "gorak-builtin:/StringObject.w4gl"
    );
    assert_eq!(engine.entries.len(), 1);
}
#[test]
fn removal_cannot_leave_bindings_to_a_reused_document_slot() {
    let mut engine = Engine::default();
    engine
        .update("file:///synthetic/app/one.w4gl", "[classsource]\n===", 0)
        .unwrap();
    engine
        .update("file:///synthetic/app/two.w4gl", "[classsource]\n===", 0)
        .unwrap();
    engine.remove("file:///synthetic/app/one.w4gl");
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE a = one; b = two; {}";
    engine.update(uri, text, 0).unwrap();
    assert!(engine.definitions(uri, position(text, "one;")).is_empty());
    assert_eq!(
        engine.definitions(uri, position(text, "two;"))[0]
            .uri
            .as_ref(),
        "file:///synthetic/app/two.w4gl"
    );
}
#[test]
fn ambiguous_duplicate_methods_do_not_produce_a_guessed_definition() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[classsource]\n===\nMETHOD Save() = {}\nMETHOD Save() = {}\nMETHOD Run() = { self.Save(); }";
    engine.update(uri, text, 0).unwrap();
    assert!(engine.definitions(uri, position(text, "Save();")).len() != 1);
    assert!(
        engine
            .rename(uri, position(text, "Save();"), "Persist")
            .is_err()
    );
}
#[test]
fn conditions_are_not_assignments_and_broken_signatures_do_not_create_warnings() {
    let uri = "file:///synthetic/app/main.w4gl";
    let mut engine = Engine::default();
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE one = StringObject; two = BitmapObject; { IF one = two THEN RETURN; ENDIF; CALLPROC save(caption = 'x'); }";
    engine
        .update(
            "file:///synthetic/app/save.w4gl",
            "[proc4glsource]\n===\nPROCEDURE save(count = INTEGER) = { RETURN count;",
            0,
        )
        .unwrap();
    engine.update(uri, text, 1).unwrap();
    assert_eq!(engine.diagnostics(uri), serde_json::json!([]));
}
#[test]
fn include_edits_invalidate_expansion_and_keep_definition_in_physical_file() {
    let uri = "file:///synthetic/app/main.w4gl";
    let include = "file:///synthetic/app/locals.w4gl";
    let text =
        "[proc4glsource]\n===\nPROCEDURE main() = DECLARE\n#include locals\n{ MESSAGE caption; }";
    let mut engine = Engine::default();
    engine
        .update(include, "[scriptsource]\n===\ncaption = VARCHAR(40);", 0)
        .unwrap();
    engine.update(uri, text, 1).unwrap();
    engine.prepare();
    let at = position(text, "caption;");
    assert_eq!(engine.definitions(uri, at)[0].uri.as_ref(), include);
    assert!(engine.hover(uri, at).to_string().contains("VARCHAR(40)"));
    engine
        .update(include, "[scriptsource]\n===\ncaption = VARCHAR(90);", 2)
        .unwrap();
    engine.prepare();
    assert!(engine.hover(uri, at).to_string().contains("VARCHAR(90)"));
    assert!(engine.rename(uri, at, "label").is_err());
    engine.remove(include);
    engine.prepare();
    assert!(engine.definitions(uri, at).is_empty());
}
#[test]
fn method_family_rename_changes_overrides_and_typed_calls_only() {
    let mut engine = Engine::default();
    engine
        .update(
            "file:///synthetic/app/base.w4gl",
            "[classsource]\n[methods]\nSave=\"METHOD\"\n===\nMETHOD Save() = {}",
            1,
        )
        .unwrap();
    engine.update("file:///synthetic/app/child.w4gl","[classsource]\nsuperclass=\"base\"\n[methods]\nSave=\"METHOD\"\n===\nMETHOD Save() = {}",2).unwrap();
    engine
        .update(
            "file:///synthetic/app/other.w4gl",
            "[classsource]\n===\nMETHOD Save() = {}",
            3,
        )
        .unwrap();
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE a = base; b = child; c = other; { a.Save(); b.Save(); c.Save(); }";
    engine.update(uri, text, 4).unwrap();
    let edit = engine
        .rename(uri, position(text, "Save();"), "Persist")
        .unwrap();
    assert_eq!(edit["documentChanges"].as_array().unwrap().len(), 3);
    assert!(!edit.to_string().contains("other.w4gl"));
    assert_eq!(
        edit["documentChanges"][2]["edits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn source_coordinates_survive_text_eviction_and_mixed_unicode() {
    let text = "a🎈é\r\nnext 界\rfinal 😀z";
    let mut source = Source::new("file:///synthetic/unicode", text).unwrap();
    let positions = text
        .char_indices()
        .map(|(i, _)| (i as u32, source.position(i as u32)))
        .collect::<Vec<_>>();
    source.release_text();
    assert!(!source.is_resident());
    for (byte, position) in positions {
        assert_eq!(source.position(byte), position);
        if !matches!(text.as_bytes()[byte as usize], b'\r' | b'\n') {
            assert_eq!(source.offset(position), byte);
        }
    }
    assert_eq!(
        source.offset(Position {
            line: 0,
            character: 2
        }),
        1
    );
}
#[test]
fn malformed_and_unicode_source_never_panics_during_editor_queries() {
    let mut state = 0x5a17_u64;
    let fragments = [
        "name", "METHOD", "DECLARE", "=", "{", "}", "(", ")", "[", "]", "'", "/*", "\n", "🎈", "é",
        "#", "$", "IF", "END", ".", ":", " ",
    ];
    for _ in 0..800 {
        let mut text = String::from("[classsource]\n===\n");
        for _ in 0..30 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push_str(fragments[(state >> 32) as usize % fragments.len()]);
        }
        let uri = "file:///synthetic/malformed.w4gl";
        let mut engine = Engine::default();
        let id = engine.update(uri, &text, 1).unwrap();
        let positions = engine
            .document(id)
            .tokens
            .iter()
            .filter(|t| t.kind == gorak_lsp_rs::syntax::lexer::Kind::Name)
            .map(|t| engine.document(id).source.position(t.span.start))
            .collect::<Vec<_>>();
        for position in positions {
            engine.definitions(uri, position);
            engine.hover(uri, position);
            engine.signature_help(uri, position);
            engine.completions(uri, position);
        }
        engine.diagnostics(uri);
        engine.document_symbols(uri);
    }
}
#[test]
fn metadata_reference_and_extended_identifiers_are_navigable() {
    let mut engine = Engine::default();
    engine
        .update("file:///synthetic/app/item.w4gl", "[classsource]\n===", 0)
        .unwrap();
    let uri = "file:///synthetic/app/holder.w4gl";
    let text = "[classsource]\n[attributes]\n\"item$value\" = \"item\"\n===\nMETHOD Read() = { MESSAGE self.item$value; }";
    engine.update(uri, text, 1).unwrap();
    let definition = engine.definitions(uri, position(text, "item\"\n"));
    assert_eq!(
        definition[0].uri.as_ref(),
        "file:///synthetic/app/item.w4gl"
    );
    assert_eq!(
        engine
            .references(uri, position(text, "item$value;"), true, false)
            .len(),
        2
    );
}

#[test]
fn sql_recovery_cannot_swallow_the_next_routine() {
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = { SELECT code FROM records }\nPROCEDURE NextStep() = DECLARE value = INTEGER; { MESSAGE value; }";
    let mut engine = Engine::default();
    engine.update(uri, text, 1).unwrap();
    let outline = engine.document_symbols(uri).to_string();
    assert!(outline.contains("NextStep"));
    assert_eq!(engine.definitions(uri, position(text, "value; }")).len(), 1);
}

#[test]
fn wml_literal_guards_use_decoded_script_values() {
    let text = "<frame><script>INITIALIZE = { MESSAGE &#39;Sa&#118;e&#39;; }</script></frame>";
    let source = Source::new("file:///synthetic/app/main.wml", text).unwrap();
    let doc = gorak_lsp_rs::syntax::parse(source);
    assert!(doc.errors.is_empty(), "{:?}", doc.errors);
    assert!(doc.facts.literal_names.contains("save"));
}

#[test]
fn qualified_metadata_types_participate_in_class_references() {
    let mut engine = Engine::default();
    engine.graph.set(
        "/synthetic/app".into(),
        r#"{"included_applications":["library"]}"#,
    );
    let class = "file:///synthetic/library/record.w4gl";
    engine.update(class, "[classsource]\n===", 0).unwrap();
    let use_uri = "file:///synthetic/app/envelope.w4gl";
    let text = "[classsource]\n[attributes]\nitem = \"library!record\"\n===";
    engine.update(use_uri, text, 0).unwrap();
    let at = position(text, "record");
    assert_eq!(engine.definitions(use_uri, at)[0].uri.as_ref(), class);
    assert!(
        engine
            .references(
                class,
                Position {
                    line: 0,
                    character: 2
                },
                false,
                false
            )
            .iter()
            .any(|r| r.uri.as_ref() == use_uri)
    );
}

#[test]
fn source_larger_than_two_megabytes_retains_trailing_declarations() {
    let uri = "file:///synthetic/app/large.w4gl";
    let mut text = String::from("[proc4glsource]\n===\n");
    for _ in 0..40_000 {
        text.push_str("// Independently generated padding for a large source fixture.\n");
    }
    text.push_str(
        "PROCEDURE large() = DECLARE trailing_value = VARCHAR(120); { MESSAGE trailing_value; }",
    );
    assert!(text.len() > 2 * 1024 * 1024);
    let at = Source::new(uri, text.as_str())
        .unwrap()
        .position(text.rfind("trailing_value;").unwrap() as u32);
    let mut engine = Engine::default();
    engine.update(uri, &text, 1).unwrap();
    assert_eq!(engine.definitions(uri, at).len(), 1);
    assert!(engine.hover(uri, at).to_string().contains("VARCHAR(120)"));
}

#[test]
fn damaged_event_header_cannot_turn_string_arguments_into_declarations() {
    let text = "[framesource]\n===\nON USEREVENT 'ready' =\nCurFrame.RegisterUserEvent(eventname = 'incoming');\n}";
    let mut engine = Engine::default();
    let id = engine
        .update("file:///synthetic/app/main.w4gl", text, 0)
        .unwrap();
    assert!(
        !engine
            .document(id)
            .symbols
            .iter()
            .any(|symbol| symbol.spelling == "eventname")
    );
}

#[test]
fn component_identity_decodes_file_uris_and_metadata_explanations_resolve() {
    let uri = "file:///synthetic/app/record%23.w4gl";
    let mut engine = Engine::default();
    let id = engine.update(uri, "[classsource]\n===", 0).unwrap();
    assert_eq!(engine.document(id).component, "record#");
    assert_eq!(
        engine.explain(
            uri,
            Position {
                line: 0,
                character: 2
            }
        )["status"],
        "resolved"
    );
}

#[test]
fn array_types_allow_language_whitespace_between_keywords() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE items = ARRAY\t OF   StringObject; { items[1].LocateString(); }";
    let id = engine.update(uri, text, 0).unwrap();
    let variable = engine
        .document(id)
        .symbols
        .iter()
        .find(|s| s.spelling == "items")
        .unwrap();
    assert!(variable.ty.as_ref().unwrap().array);
    assert_eq!(
        engine
            .definitions(uri, position(text, "LocateString("))
            .len(),
        1
    );
}

#[test]
fn included_declaration_references_are_unique_in_authoring_source() {
    let mut engine = Engine::default();
    let text =
        "[proc4glsource]\n===\nPROCEDURE main() = DECLARE\n#include locals\n{ MESSAGE caption; }";
    let first = "file:///synthetic/app/main.w4gl";
    engine
        .update(
            "file:///synthetic/app/locals.w4gl",
            "[scriptsource]\n===\ncaption = VARCHAR(40);",
            0,
        )
        .unwrap();
    engine.update(first, text, 0).unwrap();
    engine
        .update("file:///synthetic/app/second.w4gl", text, 0)
        .unwrap();
    engine.prepare();
    let locations = engine.references(first, position(text, "caption;"), true, false);
    assert_eq!(locations.len(), 3);
}

#[test]
fn parameter_completion_spaces_commas_and_preserves_metadata_case() {
    let mut engine = Engine::default();
    engine.update("file:///synthetic/app/widget.w4gl", "[classsource]\n[methods]\nsaveRecord = \"METHOD RETURNING INTEGER\"\n===\nMETHOD SaveRecord(first = INTEGER, second = INTEGER) = { RETURN 1; }", 1).unwrap();
    let uri = "file:///synthetic/app/main.w4gl";
    for (gap, prefix, space) in [
        ("", "", " "),
        ("", "sec", " "),
        (" ", "sec", ""),
        ("\n", "sec", ""),
        (" ", "", ""),
    ] {
        let text = format!(
            "[proc4glsource]\n===\nPROCEDURE main() = DECLARE item = widget; {{ item.saveRecord(first = 1,{gap}{prefix}); }}"
        );
        engine.update(uri, &text, 1).unwrap();
        let items = engine.completions(uri, position(&text, ");"));
        let item = items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["label"] == "second")
            .unwrap();
        assert_eq!(item["textEdit"]["newText"], format!("{space}second = "));
        let source = Source::new(uri, text.as_str()).unwrap();
        let range: gorak_lsp_rs::source::Range =
            serde_json::from_value(item["textEdit"]["range"].clone()).unwrap();
        assert_eq!(
            source.offset(range.end) - source.offset(range.start),
            prefix.len() as u32
        );
    }
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE item = widget; { item.sav; }";
    engine.update(uri, text, 1).unwrap();
    let items = engine.completions(uri, position(text, "; }"));
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["label"] == "saveRecord")
    );
    assert!(
        !items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["label"] == "SaveRecord")
    );
}

#[test]
fn declared_constant_values_appear_in_hover_and_completion() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/main.w4gl";
    for (metadata, expected) in [
        ("datatype = \"integer\"\ndefaultstring = \"42\"", "42"),
        ("datatype = \"integer\"\ndefaultvalue = \"0\"", "0"),
        (
            "datatype = \"varchar(40)\"\ndefaultstring = \"O'Brien\"",
            "'O''Brien'",
        ),
        ("datatype = \"varchar(40)\"\ndefaultstring = \"\"", "''"),
    ] {
        engine
            .update(
                "file:///synthetic/app/setting.w4gl",
                &format!("[constsource]\n{metadata}"),
                1,
            )
            .unwrap();
        let text = "[proc4glsource]\n===\nPROCEDURE main() = { MESSAGE setting; }";
        engine.update(uri, text, 1).unwrap();
        assert!(
            engine.hover(uri, position(text, "setting"))["contents"]["value"]
                .as_str()
                .unwrap()
                .contains(&format!(" = {expected}"))
        );
        let items = engine.completions(uri, position(text, "; }"));
        let item = items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["label"] == "setting")
            .unwrap();
        assert!(
            item["detail"]
                .as_str()
                .unwrap()
                .ends_with(&format!(" = {expected}"))
        );
    }
}

#[test]
fn constant_values_survive_summary_cache_restore() {
    use gorak_lsp_rs::cache::Cache;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("limit.w4gl");
    std::fs::write(
        &path,
        "[constsource]\ndatatype = \"integer\"\ndefaultstring = \"42\"",
    )
    .unwrap();
    let cache = Cache::new(directory.path().join("cache"));
    assert!(!cache.parse_file(&path).unwrap().hit);
    let restored = cache.parse_file(&path).unwrap();
    assert!(restored.hit);
    assert!(!restored.document.source.is_resident());
    assert_eq!(
        restored.document.symbols[0].constant_value.as_deref(),
        Some("42")
    );
}

#[test]
fn filesystem_and_editor_uris_share_application_identity() {
    let directory = tempfile::tempdir().unwrap();
    let app = directory.path().join("Example space");
    std::fs::create_dir(&app).unwrap();
    let path = app.join("record.w4gl");
    std::fs::write(&path, "[classsource]\n===").unwrap();
    let uri = url::Url::from_file_path(&path).unwrap();
    let mut engine = Engine::default();
    engine
        .update(uri.as_str(), "[classsource]\n===", 1)
        .unwrap();
    engine.graph.set(app.to_string_lossy().into_owned(), "{}");
    assert_eq!(
        engine.graph.applications.len(),
        1,
        "{:?}",
        engine.graph.applications
    );
    assert!(engine.id(uri.as_str()).is_some());
}
