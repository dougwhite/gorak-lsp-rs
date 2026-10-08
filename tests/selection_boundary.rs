use gorak_lsp_rs::{analysis::Engine, source::Source};

#[test]
fn selected_identifiers_resolve_at_punctuation_boundaries_without_moving_to_members() {
    let mut engine = Engine::default();
    let uri = "file:///synthetic/app/panel.w4gl";
    let frame_uri = "file:///synthetic/app/panel.wml";
    let frame = r#"<frame><topform><subform name="details"><entryfield name="caption"/></subform></topform></frame>"#;
    let script = "[framesource]\r\n===\r\nINITIALIZE = { MESSAGE '😀'; MESSAGE details.caption; MESSAGE details ; }";
    engine.update(frame_uri, frame, 1).unwrap();
    engine.update(uri, script, 1).unwrap();
    let source = Source::new(uri, script).unwrap();
    let start = script.find("details.caption").unwrap() as u32;
    let expected = engine.references(uri, source.position(start), true, false);
    assert_eq!(expected.len(), 3);
    for delta in [0, 2, 7] {
        assert_eq!(
            engine.references(uri, source.position(start + delta), true, false),
            expected
        );
    }
    let member_start = start + 8;
    let member = engine.definitions(uri, source.position(member_start));
    assert_eq!(member.len(), 1);
    assert_ne!(member, engine.definitions(uri, source.position(start + 7)));
    assert_eq!(
        engine.definitions(uri, source.position(member_start + 7)),
        member
    );
    assert!(
        engine
            .definitions(uri, source.position(member_start + 8))
            .is_empty()
    );
    let spaced_end = script.find("details ;").unwrap() as u32 + 7;
    assert_eq!(
        engine.references(uri, source.position(spaced_end), true, false),
        expected
    );
    assert!(
        engine
            .references(uri, source.position(spaced_end + 1), true, false)
            .is_empty()
    );
}
