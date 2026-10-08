use gorak_lsp_rs::analysis::Engine;

#[test]
fn pairs_frames_once_with_source_metadata_in_either_load_order() {
    for frame_first in [false, true] {
        let mut engine = Engine::default();
        let source = ("file:///workspace/sales/panel.w4gl", "[frametemplate]\n===");
        let frame = ("file:///workspace/sales/panel.wml", "<frame/>");
        let documents = if frame_first {
            [frame, source]
        } else {
            [source, frame]
        };
        for (uri, text) in documents {
            engine.update(uri, text, 0).unwrap();
        }
        let catalogue = engine.component_catalogue();
        assert_eq!(catalogue.len(), 1);
        let component = &catalogue[0];
        assert_eq!(component.id, "file:///workspace/sales/panel");
        assert_eq!(component.project_uri, "file:///workspace/");
        assert_eq!(component.application_uri, "file:///workspace/sales/");
        assert_eq!(component.application, "sales");
        assert_eq!(component.name, "panel");
        assert_eq!(component.component_type, "frametemplate");
        assert_eq!(component.source_uri, source.0);
        assert_eq!(component.frame_uri.as_deref(), Some(frame.0));
    }
}

#[test]
fn includes_all_component_types_and_invalid_source_without_relying_on_symbols() {
    let mut engine = Engine::default();
    let kinds = [
        "framesource",
        "frametemplate",
        "classsource",
        "proc4glsource",
        "proc3glsource",
        "globsource",
        "constsource",
        "scriptsource",
        "ghostsource",
        "extlibsource",
        "fieldtemplate",
    ];
    for kind in kinds {
        engine
            .update(
                &format!("file:///workspace/app/{kind}.w4gl"),
                &format!("[{kind}]\n==="),
                0,
            )
            .unwrap();
    }
    engine
        .update("file:///workspace/app/broken.w4gl", "[", 0)
        .unwrap();
    // Index detail eviction must not affect project browsing.
    for entry in &mut engine.entries {
        entry.document.symbols.clear();
    }
    let catalogue = engine.component_catalogue();
    assert_eq!(catalogue.len(), kinds.len() + 1);
    for kind in kinds {
        assert!(
            catalogue
                .iter()
                .any(|c| c.name == kind && c.component_type == kind)
        );
    }
    let broken = catalogue.iter().find(|c| c.name == "broken").unwrap();
    assert_eq!(broken.component_type, "unknown");
    assert!(broken.frame_uri.is_none());
    assert!(
        serde_json::to_value(broken)
            .unwrap()
            .get("frameUri")
            .is_none()
    );
}

#[test]
fn duplicate_names_keep_project_and_application_identity_and_uri_encoding() {
    let mut engine = Engine::default();
    for uri in [
        "file:///project%20one/shared/panel.w4gl",
        "file:///project%20two/shared/panel.w4gl",
        "file:///project%20one/other/panel.w4gl",
    ] {
        engine.update(uri, "[framesource]\n===", 0).unwrap();
    }
    let catalogue = engine.component_catalogue();
    assert_eq!(catalogue.len(), 3);
    assert_eq!(
        catalogue
            .iter()
            .map(|c| &c.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        catalogue
            .iter()
            .filter(|c| c.application == "shared")
            .count(),
        2
    );
    assert!(
        catalogue
            .iter()
            .any(|c| c.project_uri == "file:///project%20two/")
    );
    assert!(catalogue.iter().all(|c| c.name == "panel"));
}

#[test]
fn update_remove_and_recreate_do_not_leave_stale_pairs_or_types() {
    let mut engine = Engine::default();
    let source = "file:///workspace/app/panel.w4gl";
    let frame = "file:///workspace/app/panel.wml";
    engine.update(frame, "<frame/>", 0).unwrap();
    let orphan = engine.component_catalogue().pop().unwrap();
    assert_eq!(orphan.source_uri, frame);
    engine.update(source, "[framesource]\n===", 1).unwrap();
    assert_eq!(engine.component_catalogue()[0].id, orphan.id);
    engine.update(source, "[frametemplate]\n===", 2).unwrap();
    assert_eq!(
        engine.component_catalogue()[0].component_type,
        "frametemplate"
    );
    engine.remove(frame);
    assert!(engine.component_catalogue()[0].frame_uri.is_none());
    engine.remove(source);
    assert!(engine.component_catalogue().is_empty());
    engine.update(source, "[proc4glsource]\n===", 3).unwrap();
    assert_eq!(
        engine.component_catalogue()[0].component_type,
        "proc4glsource"
    );
}

#[test]
fn catalogue_only_types_do_not_become_callable_components() {
    let mut engine = Engine::default();
    for kind in ["extlibsource", "fieldtemplate"] {
        let name = format!("sample_{kind}");
        let uri = format!("file:///workspace/app/{name}.w4gl");
        let id = engine.update(&uri, &format!("[{kind}]\n==="), 0).unwrap();
        assert!(engine.document(id).symbols.is_empty());
        assert!(engine.document(id).errors.is_empty());
        assert!(engine.components(id, &name).is_empty());
    }
    engine
        .update(
            "file:///workspace/app/sample_fieldtemplate.wml",
            "<frame/>",
            0,
        )
        .unwrap();
    let catalogue = engine.component_catalogue();
    assert_eq!(catalogue.len(), 2);
    let template = catalogue
        .iter()
        .find(|c| c.name == "sample_fieldtemplate")
        .unwrap();
    assert_eq!(template.component_type, "fieldtemplate");
    assert_eq!(
        template.frame_uri.as_deref(),
        Some("file:///workspace/app/sample_fieldtemplate.wml")
    );
}
