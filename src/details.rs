//! Workspace-owned LRU for source text and tokens. Symbol summaries stay resident.
use crate::{analysis::Engine, cache::Cache, source::Source, syntax};
use std::collections::{HashMap, HashSet, VecDeque};

pub struct Details {
    budget: usize,
    clock: u64,
    touched: HashMap<String, u64>,
    cache: Option<Cache>,
    queue: VecDeque<(String, u64)>,
}
impl Details {
    pub fn new(budget: usize, cache: Option<Cache>) -> Self {
        Self {
            budget,
            clock: 0,
            touched: HashMap::new(),
            cache,
            queue: VecDeque::new(),
        }
    }
    pub fn load(&mut self, engine: &mut Engine, uri: &str) -> anyhow::Result<()> {
        self.observe(uri);
        if engine
            .id(uri)
            .is_some_and(|id| engine.document(id).source.is_resident())
        {
            return Ok(());
        }
        let path = url::Url::parse(uri)?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Only local workspace source can be loaded"))?;
        let text = std::fs::read_to_string(path)?;
        let source = Source::new(uri, text).map_err(anyhow::Error::msg)?;
        let document = match &self.cache {
            Some(cache) => cache.parse(source).document,
            None => syntax::parse(source),
        };
        engine.hydrate(document).map_err(anyhow::Error::msg)?;
        Ok(())
    }
    pub fn load_named(&mut self, engine: &mut Engine, name: &str) -> anyhow::Result<()> {
        let uris = engine
            .entries
            .iter()
            .filter(|e| e.document.names.find(name).is_some())
            .map(|e| e.document.source.uri.to_string())
            .collect::<Vec<_>>();
        for uri in uris {
            self.load(engine, &uri)?;
        }
        Ok(())
    }
    pub fn observe(&mut self, uri: &str) {
        self.clock += 1;
        self.touched.insert(uri.into(), self.clock);
        self.queue.push_back((uri.into(), self.clock));
        if self.queue.len() > self.touched.len().saturating_mul(2) + 64 {
            self.queue
                .retain(|(uri, age)| self.touched.get(uri) == Some(age));
        }
    }
    pub fn bytes(&self, engine: &Engine) -> usize {
        engine.resident_detail_bytes
    }
    pub fn trim(&mut self, engine: &mut Engine, open: &HashSet<String>) {
        for uri in std::mem::take(&mut engine.changed_details) {
            self.observe(&uri);
        }
        let mut pinned = VecDeque::new();
        while engine.resident_detail_bytes > self.budget {
            let Some((uri, age)) = self.queue.pop_front() else {
                break;
            };
            if self.touched.get(&uri) != Some(&age) {
                continue;
            }
            if open.contains(&uri) || open.iter().any(|item| engine.id(item) == engine.id(&uri)) {
                pinned.push_back((uri, age));
                continue;
            }
            if let Some(id) = engine.id(&uri) {
                engine.evict(id);
            }
        }
        self.queue.extend(pinned);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicted_documents_keep_locations_and_hydrate_without_changing_bindings() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("main.w4gl");
        let text = "[proc4glsource]\n===\nPROCEDURE main() = DECLARE caption = VARCHAR(40); { MESSAGE caption; }";
        std::fs::write(&file, text).unwrap();
        let uri = url::Url::from_file_path(file).unwrap().to_string();
        let mut engine = Engine::default();
        let id = engine.update(&uri, text, 0).unwrap();
        let at = engine
            .document(id)
            .source
            .position(text.rfind("caption").unwrap() as u32);
        let before = engine.definitions(&uri, at);
        let mut details = Details {
            budget: 0,
            clock: 0,
            touched: HashMap::new(),
            cache: None,
            queue: VecDeque::new(),
        };
        details.observe(&uri);
        details.trim(&mut engine, &HashSet::new());
        assert_eq!(details.bytes(&engine), 0);
        assert!(!engine.document(id).source.is_resident());
        assert!(!engine.document_symbols(&uri).as_array().unwrap().is_empty());
        details.load(&mut engine, &uri).unwrap();
        assert_eq!(engine.definitions(&uri, at), before);
        let open = HashSet::from([uri]);
        details.trim(&mut engine, &open);
        assert!(engine.document(id).source.is_resident());
    }
}
