//! Background disk discovery never mutates the editor's analysis snapshot.
use crate::{
    source::Source,
    syntax::{self, model::Document},
};
use crossbeam_channel::{Receiver, bounded};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use walkdir::WalkDir;

pub enum ScanEvent {
    Application {
        directory: String,
        text: String,
    },
    Document {
        document: Box<Document>,
        cached: bool,
    },
    Failed(String),
    Complete,
}
pub struct Scan {
    pub events: Receiver<ScanEvent>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Scan {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
impl Scan {
    pub fn start(roots: Vec<PathBuf>, cache: Option<crate::cache::Cache>) -> Self {
        let (sender, events) = bounded(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        thread::spawn(move || {
            let mut seen = std::collections::HashSet::new();
            for root in roots {
                let walker = WalkDir::new(root)
                    .sort_by_file_name()
                    .into_iter()
                    .filter_entry(|entry| {
                        !entry.file_type().is_dir()
                            || entry.depth() == 0
                            || !excluded(entry.file_name().to_string_lossy().as_ref())
                    });
                for entry in walker {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let event = match entry {
                        Err(error) => Some(ScanEvent::Failed(error.to_string())),
                        Ok(entry)
                            if entry.file_type().is_file()
                                && seen.insert(entry.path().to_path_buf()) =>
                        {
                            load_cached(entry.path(), cache.as_ref())
                        }
                        _ => None,
                    };
                    if let Some(event) = event
                        && sender.send(event).is_err()
                    {
                        return;
                    }
                }
            }
            let _ = sender.send(ScanEvent::Complete);
        });
        Self { events, cancelled }
    }
}
fn excluded(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "node_modules" | "target")
}
pub fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("w4gl") || s.eq_ignore_ascii_case("wml"))
}
pub fn load(path: &Path) -> Option<ScanEvent> {
    load_cached(path, None)
}
fn load_cached(path: &Path, cache: Option<&crate::cache::Cache>) -> Option<ScanEvent> {
    if !is_source(path) && path.file_name().is_none_or(|s| s != "app.json") {
        return None;
    }
    if is_source(path)
        && let Some(cache) = cache
    {
        return Some(match cache.parse_file(path) {
            Ok(parsed) => ScanEvent::Document {
                document: Box::new(parsed.document),
                cached: parsed.hit,
            },
            Err(error) => {
                ScanEvent::Failed(format!("Cannot load source {}: {error}", path.display()))
            }
        });
    }
    Some(match std::fs::read_to_string(path) {
        Err(error) => ScanEvent::Failed(format!("Cannot read {}: {error}", path.display())),
        Ok(text) if path.file_name().is_some_and(|s| s == "app.json") => ScanEvent::Application {
            directory: url::Url::from_file_path(path)
                .ok()
                .map(|uri| {
                    crate::analysis::index::application(&crate::source::canonical_uri(uri.as_str()))
                })
                .unwrap_or_default(),
            text,
        },
        Ok(text) => match url::Url::from_file_path(path) {
            Err(_) => ScanEvent::Failed("Workspace path must be absolute".into()),
            Ok(uri) => match Source::new(uri.as_str(), text) {
                Ok(source) => {
                    let parsed = cache.map_or_else(
                        || crate::cache::Parsed {
                            document: syntax::parse(source.clone()),
                            hit: false,
                        },
                        |cache| cache.parse(source.clone()),
                    );
                    ScanEvent::Document {
                        document: Box::new(parsed.document),
                        cached: parsed.hit,
                    }
                }
                Err(error) => ScanEvent::Failed(error.into()),
            },
        },
    })
}
