//! Ordered direct application visibility. Image dependencies never acquire guessed source.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Include {
    pub name: String,
    pub image: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Application {
    pub directory: String,
    pub includes: Vec<Include>,
    pub valid: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LookupIssue {
    InvalidApplicationMetadata,
    IncludeCycle,
    InclusionKindConflict,
    UnboundImage,
    AmbiguousApplication,
    MissingApplication,
}
#[derive(Clone, Debug)]
pub struct Lookup<T> {
    pub candidates: Vec<T>,
    pub issues: Vec<LookupIssue>,
}
impl<T> Lookup<T> {
    fn found(candidates: Vec<T>) -> Self {
        Self {
            candidates,
            issues: Vec::new(),
        }
    }
    fn blocked(issue: LookupIssue) -> Self {
        Self {
            candidates: Vec::new(),
            issues: vec![issue],
        }
    }
}
#[derive(Default)]
pub struct Graph {
    pub applications: HashMap<String, Application>,
    by_name: HashMap<(String, String), Vec<String>>,
}
fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_string_lossy()
        .into_owned()
}
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
impl Graph {
    pub fn set(&mut self, directory: String, text: &str) {
        let directory = url::Url::from_directory_path(&directory)
            .ok()
            .and_then(|uri| crate::source::uri_path(&crate::source::canonical_uri(uri.as_str())))
            .map(|path| {
                path.to_string_lossy()
                    .trim_end_matches(std::path::MAIN_SEPARATOR)
                    .to_owned()
            })
            .unwrap_or(directory);
        let mut app = Application {
            directory: directory.clone(),
            includes: Vec::new(),
            valid: true,
        };
        let value: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                app.valid = false;
                self.insert(app);
                return;
            }
        };
        if !value.is_object() {
            app.valid = false;
        }
        if let Some(includes) = value.get("included_applications") {
            if let Some(rows) = includes.as_array() {
                for row in rows {
                    let name = row
                        .as_str()
                        .or_else(|| row.get("name").and_then(serde_json::Value::as_str));
                    let image = row.get("image");
                    if let Some(name) = name.filter(|s| {
                        !s.trim().is_empty()
                            && !s.contains(['/', '\\'])
                            && !matches!(*s, "." | "..")
                    }) {
                        if image.is_some_and(|v| !v.is_string()) {
                            app.valid = false;
                            continue;
                        }
                        app.includes.push(Include {
                            name: name.into(),
                            image: image
                                .and_then(serde_json::Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned),
                        });
                    } else {
                        app.valid = false;
                    }
                }
            } else {
                app.valid = false;
            }
        }
        self.insert(app);
    }
    pub fn ensure(&mut self, directory: String) {
        if !self.applications.contains_key(&directory) {
            self.insert(Application {
                directory,
                includes: Vec::new(),
                valid: true,
            });
        }
    }
    pub fn insert(&mut self, app: Application) {
        let key = (
            parent(&app.directory),
            basename(&app.directory).to_ascii_lowercase(),
        );
        let entries = self.by_name.entry(key).or_default();
        if !entries.contains(&app.directory) {
            entries.push(app.directory.clone());
        }
        self.applications.insert(app.directory.clone(), app);
    }
    fn sources(&self, app: &str, name: &str) -> &[String] {
        self.by_name
            .get(&(parent(app), name.to_ascii_lowercase()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub fn hierarchy_issue(&self, app: &str) -> Option<LookupIssue> {
        fn visit(
            graph: &Graph,
            app: &str,
            active: &mut HashSet<String>,
            seen: &mut HashSet<String>,
            kinds: &mut HashMap<String, bool>,
        ) -> Option<LookupIssue> {
            if active.contains(app) {
                return Some(LookupIssue::IncludeCycle);
            }
            if !seen.insert(app.into()) {
                return None;
            }
            active.insert(app.into());
            if let Some(node) = graph.applications.get(app) {
                for include in &node.includes {
                    let image = include.image.is_some();
                    if kinds
                        .insert(include.name.to_ascii_lowercase(), image)
                        .is_some_and(|prior| prior != image)
                    {
                        return Some(LookupIssue::InclusionKindConflict);
                    }
                    let sources = graph.sources(app, &include.name);
                    if !image
                        && sources.len() == 1
                        && let Some(issue) = visit(graph, &sources[0], active, seen, kinds)
                    {
                        return Some(issue);
                    }
                }
            }
            active.remove(app);
            None
        }
        visit(
            self,
            app,
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut HashMap::new(),
        )
    }
    pub fn lookup<T>(
        &self,
        app: &str,
        qualifier: Option<&str>,
        mut candidates: impl FnMut(&str) -> Vec<T>,
    ) -> Lookup<T> {
        let own = candidates(app);
        if qualifier.is_some_and(|q| q.eq_ignore_ascii_case(&basename(app)))
            || qualifier.is_none() && !own.is_empty()
        {
            return Lookup::found(own);
        }
        if self.applications.get(app).is_some_and(|a| !a.valid) {
            return Lookup::blocked(LookupIssue::InvalidApplicationMetadata);
        }
        if let Some(issue) = self.hierarchy_issue(app) {
            return Lookup::blocked(issue);
        }
        if let Some(node) = self.applications.get(app) {
            for edge in &node.includes {
                if qualifier.is_some_and(|q| !q.eq_ignore_ascii_case(&edge.name)) {
                    continue;
                }
                if edge.image.is_some() {
                    return Lookup::blocked(LookupIssue::UnboundImage);
                }
                let sources = self.sources(app, &edge.name);
                if sources.len() != 1 {
                    return Lookup::blocked(if sources.is_empty() {
                        LookupIssue::MissingApplication
                    } else {
                        LookupIssue::AmbiguousApplication
                    });
                }
                let found = candidates(&sources[0]);
                if !found.is_empty() || qualifier.is_some() {
                    return Lookup::found(found);
                }
            }
        }
        Lookup::found(Vec::new())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_order_and_missing_image_boundaries() {
        let mut g = Graph::default();
        g.set(
            "/p/main".into(),
            r#"{"included_applications":["one","two"]}"#,
        );
        g.set("/p/one".into(), r#"{"included_applications":["hidden"]}"#);
        g.ensure("/p/two".into());
        g.ensure("/p/hidden".into());
        let lookup = g.lookup("/p/main", None, |app| {
            if app == "/p/hidden" { vec![1] } else { vec![] }
        });
        assert!(lookup.candidates.is_empty());
        let lookup = g.lookup("/p/main", None, |app| {
            if app == "/p/one" {
                vec![1]
            } else if app == "/p/two" {
                vec![2]
            } else {
                vec![]
            }
        });
        assert_eq!(lookup.candidates, vec![1]);
        g.set(
            "/p/main".into(),
            r#"{"included_applications":[{"name":"one","image":"external.img"},"two"]}"#,
        );
        assert_eq!(
            g.lookup("/p/main", None, |_| Vec::<u32>::new()).issues,
            vec![LookupIssue::UnboundImage]
        );
    }
}
