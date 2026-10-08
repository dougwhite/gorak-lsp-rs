use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};
use std::time::Duration;

fn request(client: &Connection, id: i32, method: &str, params: Value) -> Value {
    client
        .sender
        .send(Message::Request(Request::new(
            RequestId::from(id),
            method.into(),
            params,
        )))
        .unwrap();
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("server response within deadline")
        {
            Message::Response(response) if response.id == RequestId::from(id) => {
                assert!(response.error.is_none(), "{:?}", response.error);
                return response.result.unwrap();
            }
            _ => {}
        }
    }
}
fn notify(client: &Connection, method: &str, params: Value) {
    client
        .sender
        .send(Message::Notification(Notification::new(
            method.into(),
            params,
        )))
        .unwrap();
}
#[test]
fn initialize_open_edit_stale_edit_and_shutdown() {
    let (client, server) = Connection::memory();
    let worker = std::thread::spawn(move || gorak_lsp_rs::server::serve(&server));
    let initialized = request(&client, 1, "initialize", json!({"capabilities":{}}));
    assert_eq!(initialized["capabilities"]["positionEncoding"], "utf-16");
    notify(&client, "initialized", json!({}));
    let uri = "file:///synthetic/app/main.w4gl";
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE caption = VARCHAR(40); { MESSAGE caption; }";
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"openroad","version":1,"text":text}}),
    );
    let at = json!({"textDocument":{"uri":uri},"position":{"line":2,"character":60}});
    let hover = request(&client, 2, "textDocument/hover", at.clone());
    assert!(hover.to_string().contains("VARCHAR(40)"));
    notify(
        &client,
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":text.replace("VARCHAR(40)","VARCHAR(80)")}]}),
    );
    let hover = request(&client, 3, "textDocument/hover", at.clone());
    assert!(hover.to_string().contains("VARCHAR(80)"));
    notify(
        &client,
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":1},"contentChanges":[{"text":text}]}),
    );
    let hover = request(&client, 4, "textDocument/hover", at);
    assert!(hover.to_string().contains("VARCHAR(80)"));
    client
        .sender
        .send(Message::Request(Request::new(
            RequestId::from(5),
            "shutdown".into(),
            Value::Null,
        )))
        .unwrap();
    notify(&client, "exit", Value::Null);
    worker.join().unwrap().unwrap();
}

fn ready(client: &Connection) -> Value {
    for _ in 0..200 {
        let status = request(client, 90, "gorak/indexStatus", json!({}));
        if status["indexing"] == false {
            assert_eq!(status["failed"], false);
            return status;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("index did not settle");
}
fn shutdown(client: &Connection, worker: std::thread::JoinHandle<anyhow::Result<()>>) {
    client
        .sender
        .send(Message::Request(Request::new(
            99.into(),
            "shutdown".into(),
            Value::Null,
        )))
        .unwrap();
    notify(client, "exit", Value::Null);
    worker.join().unwrap().unwrap();
}

#[test]
fn restart_cache_lazy_include_loading_and_watch_invalidation() {
    let directory = tempfile::tempdir().unwrap();
    let app = directory.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let main = app.join("main.w4gl");
    let include = app.join("locals.w4gl");
    let text =
        "[proc4glsource]\n===\nPROCEDURE main() = DECLARE\n#include locals\n{ MESSAGE caption; }";
    std::fs::write(&main, text).unwrap();
    std::fs::write(&include, "[scriptsource]\n===\ncaption = VARCHAR(40);").unwrap();
    let uri = url::Url::from_file_path(&main).unwrap().to_string();
    let include_uri = url::Url::from_file_path(&include).unwrap().to_string();
    let params = json!({"rootUri":url::Url::from_directory_path(&app).unwrap().to_string(),"capabilities":{},"initializationOptions":{"cacheDirectory":directory.path().join("cache"),"memoryBudgetMB":8}});
    for restart in 0..2 {
        let (client, server) = Connection::memory();
        let worker = std::thread::spawn(move || gorak_lsp_rs::server::serve(&server));
        request(&client, 1, "initialize", params.clone());
        notify(&client, "initialized", json!({}));
        let status = ready(&client);
        assert_eq!(status["files"], 2);
        assert_eq!(status["restoredFiles"], if restart == 0 { 0 } else { 2 });
        let at = json!({"textDocument":{"uri":uri},"position":{"line":4,"character":12}});
        let definitions = request(&client, 2, "textDocument/definition", at.clone());
        assert_eq!(
            definitions[0]["uri"],
            gorak_lsp_rs::source::canonical_uri(&include_uri)
        );
        assert!(
            request(&client, 3, "textDocument/hover", at.clone())
                .to_string()
                .contains("VARCHAR(40)")
        );
        if restart == 1 {
            std::fs::write(&include, "[scriptsource]\n===\ncaption = VARCHAR(80);").unwrap();
            notify(
                &client,
                "workspace/didChangeWatchedFiles",
                json!({"changes":[{"uri":include_uri,"type":2}]}),
            );
            assert!(
                request(&client, 4, "textDocument/hover", at)
                    .to_string()
                    .contains("VARCHAR(80)")
            );
        }
        shutdown(&client, worker);
    }
}

#[test]
fn metadata_overlay_survives_rebuild_and_close_restores_disk_graph() {
    let directory = tempfile::tempdir().unwrap();
    let app = directory.path().join("app");
    let library = directory.path().join("library");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(&library).unwrap();
    std::fs::write(app.join("app.json"), "{}").unwrap();
    std::fs::write(library.join("record.w4gl"), "[classsource]\n===").unwrap();
    let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE item = record; { MESSAGE item; }";
    std::fs::write(app.join("main.w4gl"), text).unwrap();
    let uri = url::Url::from_file_path(app.join("main.w4gl"))
        .unwrap()
        .to_string();
    let metadata = url::Url::from_file_path(app.join("app.json"))
        .unwrap()
        .to_string();
    let (client, server) = Connection::memory();
    let worker = std::thread::spawn(move || gorak_lsp_rs::server::serve(&server));
    request(
        &client,
        1,
        "initialize",
        json!({"rootUri":url::Url::from_directory_path(directory.path()).unwrap().to_string(),"capabilities":{},"initializationOptions":{"cacheDirectory":directory.path().join("cache")}}),
    );
    notify(&client, "initialized", json!({}));
    ready(&client);
    let position = gorak_lsp_rs::source::Source::new(&*uri, text)
        .unwrap()
        .position(text.find("record;").unwrap() as u32);
    let at = json!({"textDocument":{"uri":uri},"position":position});
    assert_eq!(
        request(&client, 2, "textDocument/definition", at.clone()),
        json!([])
    );
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":metadata,"version":1,"text":"{\"included_applications\":[\"library\"]}"}}),
    );
    assert_eq!(
        request(&client, 3, "textDocument/definition", at.clone())
            .as_array()
            .unwrap()
            .len(),
        1,
        "{}",
        request(&client, 33, "gorak/resolve", at.clone())
    );
    request(&client, 4, "gorak/rebuildIndex", json!({}));
    assert_eq!(
        request(&client, 5, "textDocument/definition", at.clone())
            .as_array()
            .unwrap()
            .len(),
        1
    );
    notify(
        &client,
        "textDocument/didClose",
        json!({"textDocument":{"uri":metadata}}),
    );
    assert_eq!(
        request(&client, 6, "textDocument/definition", at),
        json!([])
    );
    shutdown(&client, worker);
}

#[test]
fn component_catalogue_tracks_disk_overlay_and_workspace_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    for root in [&first, &second] {
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/panel.w4gl"), "[framesource]\n===").unwrap();
        std::fs::write(root.join("app/panel.wml"), "<frame/>").unwrap();
    }
    std::fs::create_dir(second.join("empty")).unwrap();
    std::fs::write(second.join("empty/app.json"), "{}").unwrap();
    let folder = |path: &std::path::Path| json!({"uri":url::Url::from_directory_path(path).unwrap().to_string(),"name":"project"});
    let (client, server) = Connection::memory();
    let worker = std::thread::spawn(move || gorak_lsp_rs::server::serve(&server));
    request(
        &client,
        1,
        "initialize",
        json!({"capabilities":{},"workspaceFolders":[folder(&first),folder(&second)]}),
    );
    notify(&client, "initialized", json!({}));
    let result = request(&client, 2, "gorak/componentCatalogue", json!({}));
    assert_eq!(result["indexing"], false);
    assert_eq!(result["failures"], 0);
    assert_eq!(result["components"].as_array().unwrap().len(), 2);
    assert_eq!(result["applications"].as_array().unwrap().len(), 3);
    assert!(
        result["applications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["name"] == "empty")
    );
    for component in result["components"].as_array().unwrap() {
        assert!(
            result["applications"]
                .as_array()
                .unwrap()
                .iter()
                .any(|app| app["uri"] == component["applicationUri"]
                    && app["projectUri"] == component["projectUri"])
        );
    }
    assert_ne!(result["components"][0]["id"], result["components"][1]["id"]);
    let uri = url::Url::from_file_path(first.join("app/panel.w4gl"))
        .unwrap()
        .to_string();
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"openroad","version":1,"text":"[frametemplate]\n==="}}),
    );
    let result = request(&client, 3, "gorak/componentCatalogue", json!({}));
    assert!(
        result["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["componentType"] == "frametemplate")
    );
    notify(
        &client,
        "textDocument/didClose",
        json!({"textDocument":{"uri":uri}}),
    );
    let result = request(&client, 4, "gorak/componentCatalogue", json!({}));
    assert!(
        result["components"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["componentType"] == "framesource")
    );
    std::fs::remove_file(first.join("app/panel.wml")).unwrap();
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":url::Url::from_file_path(first.join("app/panel.wml")).unwrap().to_string(),"type":3}]}),
    );
    let result = request(&client, 5, "gorak/componentCatalogue", json!({}));
    assert_eq!(
        result["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c.get("frameUri").is_none())
            .count(),
        1
    );
    notify(
        &client,
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"openroad","version":2,"text":"[frametemplate]\n==="}}),
    );
    let empty_manifest = second.join("empty/app.json");
    std::fs::remove_file(&empty_manifest).unwrap();
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":url::Url::from_file_path(&empty_manifest).unwrap().to_string(),"type":3}]}),
    );
    let result = request(&client, 7, "gorak/componentCatalogue", json!({}));
    assert_eq!(result["applications"].as_array().unwrap().len(), 2);
    assert!(
        result["applications"]
            .as_array()
            .unwrap()
            .iter()
            .all(|app| app["name"] != "empty")
    );
    assert!(
        result["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|component| component["componentType"] == "frametemplate")
    );
    notify(
        &client,
        "textDocument/didClose",
        json!({"textDocument":{"uri":uri}}),
    );
    std::fs::write(&empty_manifest, "{}").unwrap();
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":url::Url::from_file_path(&empty_manifest).unwrap().to_string(),"type":1}]}),
    );
    let result = request(&client, 8, "gorak/componentCatalogue", json!({}));
    assert_eq!(result["applications"].as_array().unwrap().len(), 3);
    notify(
        &client,
        "workspace/didChangeWorkspaceFolders",
        json!({"event":{"added":[],"removed":[folder(&second)]}}),
    );
    let result = request(&client, 6, "gorak/componentCatalogue", json!({}));
    assert_eq!(result["components"].as_array().unwrap().len(), 1);
    assert_eq!(result["applications"].as_array().unwrap().len(), 1);
    assert_eq!(result["applications"][0]["name"], "app");
    assert_eq!(
        result["components"][0]["sourceUri"],
        gorak_lsp_rs::source::canonical_uri(uri.as_str())
    );
    shutdown(&client, worker);
}
