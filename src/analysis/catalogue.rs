//! Logical components from the resident document index, independent of symbol detail.
use super::Engine;
use crate::analysis::index::component_key;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub id: String,
    pub project_uri: String,
    pub application_uri: String,
    pub application: String,
    pub name: String,
    pub component_type: String,
    pub source_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_uri: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentCatalogue {
    pub components: Vec<Component>,
    pub indexing: bool,
    pub failures: usize,
}

impl Engine {
    pub fn component_catalogue(&self) -> Vec<Component> {
        let mut components = BTreeMap::<String, Component>::new();
        for entry in &self.entries {
            if self.interrupted() {
                return Vec::new();
            }
            let doc = &entry.document;
            let uri = doc.source.uri.as_ref();
            let Some(path) = crate::source::uri_path(uri) else {
                continue;
            };
            if !crate::workspace::is_source(&path) {
                continue;
            }
            let Ok(url) = url::Url::parse(uri) else {
                continue;
            };
            let (Ok(application_uri), Ok(project_uri)) = (url.join("./"), url.join("../")) else {
                continue;
            };
            let application = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let frame = crate::syntax::is_wml(uri);
            let id = component_key(uri).to_owned();
            let component_type = if doc.component_kind.is_empty() {
                "unknown"
            } else {
                &doc.component_kind
            };
            let component = components.entry(id.clone()).or_insert_with(|| Component {
                id,
                project_uri: project_uri.into(),
                application_uri: application_uri.into(),
                application,
                name: doc.component.clone(),
                component_type: component_type.to_owned(),
                source_uri: uri.to_owned(),
                frame_uri: None,
            });
            if frame {
                component.frame_uri = Some(uri.to_owned());
            } else {
                // W4GL owns component metadata; WML contributes its paired editor URI.
                component.name.clone_from(&doc.component);
                component.component_type = component_type.to_owned();
                component.source_uri = uri.to_owned();
            }
        }
        components.into_values().collect()
    }
}
