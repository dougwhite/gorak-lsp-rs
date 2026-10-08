use gorak_lsp_rs::{
    analysis::Engine,
    source::{Position, Source},
};

fn position(text: &str, needle: &str) -> Position {
    Source::new("file:///fixture", text)
        .unwrap()
        .position(text.find(needle).unwrap() as u32)
}

#[test]
fn event_targets_bind_fields_without_capturing_handler_locals_or_event_names() {
    let mut engine = Engine::default();
    let frame_uri = "file:///synthetic/app/panel.wml";
    let script_uri = "file:///synthetic/app/panel.w4gl";
    let frame = r#"<frame><topform><buttonfield name="trigger_control"/><buttonfield name="click"/><subform name="details_panel"><entryfield name="caption"/></subform></topform></frame>"#;
    let script = "[framesource]\n===\nON CLICK trigger_control, ON ENTRY details_panel.caption = DECLARE trigger_control = INTEGER; { trigger_control = 1; }\nON USEREVENT 'trigger_control' = { }\nON CLICK = { }";
    engine.update(frame_uri, frame, 1).unwrap();
    engine.update(script_uri, script, 1).unwrap();
    for (field, target) in [
        ("trigger_control", "trigger_control,"),
        ("caption", "caption ="),
    ] {
        let expected = position(frame, &format!("{field}\""));
        let definitions = engine.definitions(script_uri, position(script, target));
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].uri.as_ref(), frame_uri);
        assert_eq!(definitions[0].range.start, expected);
        let refs = engine.references(frame_uri, expected, false, false);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].uri.as_ref(), script_uri);
        assert_eq!(refs[0].range.start, position(script, target));
    }
    assert!(
        engine
            .definitions(script_uri, position(script, "CLICK trigger_control"))
            .is_empty()
    );
    assert!(
        engine
            .references(frame_uri, position(frame, "click\""), false, false)
            .is_empty()
    );
    assert_eq!(
        engine.definitions(script_uri, position(script, "trigger_control = 1"))[0]
            .uri
            .as_ref(),
        script_uri
    );
}

#[test]
fn named_tab_pages_are_field_scopes_but_unrelated_rows_are_not() {
    let mut engine = Engine::default();
    let frame_uri = "file:///synthetic/app/panel.wml";
    let script_uri = "file:///synthetic/app/panel.w4gl";
    let frame = r#"<frame><topform><tabfolder><tabpagearray><row name="address"><stackfield><entryfield name="caption"/></stackfield></row><row name="billing"><entryfield name="caption"/></row></tabpagearray></tabfolder><taggedvalues><row name="metadata"/></taggedvalues></topform></frame>"#;
    let script = "[framesource]\n===\nON ENTRY address.caption = { MESSAGE billing.caption; }";
    engine.update(frame_uri, frame, 1).unwrap();
    engine.update(script_uri, script, 1).unwrap();
    let first = position(frame, "caption\"");
    let refs = engine.references(frame_uri, first, false, false);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].range.start, position(script, "caption ="));
    let second = engine.definitions(script_uri, position(script, "caption;"));
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].range.start, first);
    assert!(
        engine
            .definitions(frame_uri, position(frame, "metadata\""))
            .is_empty()
    );
}

#[test]
fn embedded_event_targets_preserve_unicode_spans_across_comments_and_lines() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/panel.wml";
    let frame = r#"<frame><topform><entryfield name="caption"/><freetrim textlabel="🎈"/><script><![CDATA[ON ENTRY /* first */ caption,
 ON EXIT // second
 caption = { }]]></script></topform></frame>"#;
    engine.update(uri, frame, 1).unwrap();
    let field = position(frame, "caption\"");
    let refs = engine.references(uri, field, false, false);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].range.start, position(frame, "caption,"));
    assert_eq!(refs[1].range.start, position(frame, "caption ="));
    assert_eq!(
        engine.definitions(uri, refs[0].range.start)[0].range.start,
        field
    );
}
