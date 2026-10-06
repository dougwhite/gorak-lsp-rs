//! Content-addressed parse checkpoints. An interrupted scan retains completed files.
use crate::{
    source::Source,
    syntax::{self, model::Document},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

// Increment whenever parsing or serialized model semantics change.
const SCHEMA: &str = "native-parse-v17";
const MAX_CACHE_BYTES: u64 = 128 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Record {
    schema: String,
    digest: [u8; 32],
    document: Document,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    length: u64,
    seconds: u64,
    nanos: u32,
}
impl Stamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Self {
            length: metadata.len(),
            seconds: modified.as_secs(),
            nanos: modified.subsec_nanos(),
        })
    }
}
#[derive(Serialize, Deserialize)]
struct Summary {
    schema: String,
    stamp: Stamp,
    document: Document,
}
#[derive(Clone)]
pub struct Cache {
    directory: PathBuf,
}
pub struct Parsed {
    pub document: Document,
    pub hit: bool,
}
impl Cache {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn user_default() -> Option<Self> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        Some(Self::new(base.join("gorak-lsp-rs").join(SCHEMA)))
    }
    fn path(&self, uri: &str) -> PathBuf {
        self.directory
            .join(blake3::hash(uri.as_bytes()).to_hex().as_str())
    }
    pub fn parse(&self, source: Source) -> Parsed {
        let digest = source.fingerprint;
        if let Some(document) = self.read(&source, digest) {
            return Parsed {
                document,
                hit: true,
            };
        }
        let document = syntax::parse(source);
        // Cache failures cannot prevent editing. The caller can still use the parsed document.
        let _ = self.write(&document, digest);
        Parsed {
            document,
            hit: false,
        }
    }
    pub fn parse_file(&self, path: &Path) -> anyhow::Result<Parsed> {
        let uri = url::Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("Expected an absolute source path"))?;
        let uri = url::Url::parse(&crate::source::canonical_uri(uri.as_str()))?;
        let stamp = Stamp::read(path);
        if let Some(stamp) = &stamp
            && let Some(document) = self.read_summary(uri.as_str(), stamp)
        {
            return Ok(Parsed {
                document,
                hit: true,
            });
        }
        let source =
            Source::new(uri.as_str(), fs::read_to_string(path)?).map_err(anyhow::Error::msg)?;
        let parsed = self.parse(source);
        if let Some(stamp) = stamp.filter(|s| Some(s) == Stamp::read(path).as_ref()) {
            let mut document = parsed.document.clone();
            document.source.release_text();
            document.tokens.clear();
            document.tokens.shrink_to_fit();
            let summary = Summary {
                schema: SCHEMA.into(),
                stamp,
                document,
            };
            if let Ok(bytes) = postcard::to_stdvec(&summary) {
                let _ = write_checked(&self.path(uri.as_str()).with_extension("summary"), &bytes);
            }
        }
        Ok(parsed)
    }
    fn read_summary(&self, uri: &str, stamp: &Stamp) -> Option<Document> {
        let bytes = read_checked(&self.path(uri).with_extension("summary"))?;
        let summary: Summary = postcard::from_bytes(&bytes).ok()?;
        (summary.schema == SCHEMA
            && summary.stamp == *stamp
            && summary.document.source.uri.as_ref() == uri)
            .then_some(summary.document)
    }
    fn read(&self, source: &Source, digest: [u8; 32]) -> Option<Document> {
        let path = self.path(&source.uri);
        let bytes = read_checked(&path)?;
        let record: Record = postcard::from_bytes(&bytes).ok()?;
        if !record.document.source.is_resident()
            || record.schema != SCHEMA
            || record.digest != digest
            || record.document.source.uri != source.uri
            || record.document.source.text() != source.text()
        {
            return None;
        }
        Some(record.document)
    }
    fn write(&self, document: &Document, digest: [u8; 32]) -> anyhow::Result<()> {
        fs::create_dir_all(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        }
        #[derive(Serialize)]
        struct BorrowedRecord<'a> {
            schema: &'a str,
            digest: [u8; 32],
            document: &'a Document,
        }
        let bytes = postcard::to_stdvec(&BorrowedRecord {
            schema: SCHEMA,
            digest,
            document,
        })?;
        write_checked(&self.path(&document.source.uri), &bytes)
    }
}
fn read_checked(path: &Path) -> Option<Vec<u8>> {
    if fs::metadata(path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let (checksum, payload) = bytes.split_at_checked(32)?;
    (blake3::hash(payload).as_bytes() == checksum).then(|| payload.to_vec())
}
fn write_checked(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut checked = Vec::with_capacity(bytes.len() + 32);
    checked.extend_from_slice(blake3::hash(bytes).as_bytes());
    checked.extend_from_slice(bytes);
    atomic_write(path, &checked)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        drop(file);
        // An interrupted or corrupt cache is discarded; source files remain authoritative.
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoints_survive_restart_and_reject_changed_or_corrupt_content() {
        let directory = tempfile::tempdir().unwrap();
        let uri = "file:///synthetic/main.w4gl";
        let source = Source::new(uri, "[proc4glsource]\n===\nPROCEDURE main() = {}").unwrap();
        let cache = Cache::new(directory.path().into());
        assert!(!cache.parse(source.clone()).hit);
        drop(cache);
        let cache = Cache::new(directory.path().into());
        assert!(cache.parse(source.clone()).hit);
        let changed =
            Source::new(uri, "[proc4glsource]\n===\nPROCEDURE main() = { RETURN; }").unwrap();
        assert!(!cache.parse(changed.clone()).hit);
        assert!(cache.parse(changed.clone()).hit);
        fs::write(cache.path(uri), b"incomplete cache write").unwrap();
        assert!(!cache.parse(changed).hit);
        assert!(!cache.parse(source).hit);
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    #[test]
    fn restart_restores_summary_without_text_or_tokens_and_detects_disk_edits() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.w4gl");
        fs::write(
            &source,
            "[proc4glsource]\n===\nPROCEDURE main() = { RETURN; }",
        )
        .unwrap();
        let cache = Cache::new(directory.path().join("cache"));
        let first = cache.parse_file(&source).unwrap();
        assert!(!first.hit);
        assert!(first.document.source.is_resident());
        drop(cache);
        let cache = Cache::new(directory.path().join("cache"));
        let restored = cache.parse_file(&source).unwrap();
        assert!(restored.hit);
        assert!(!restored.document.source.is_resident());
        assert!(restored.document.tokens.is_empty());
        let uri = url::Url::from_file_path(&source).unwrap();
        let text = fs::read_to_string(&source).unwrap();
        let full = cache.parse(Source::new(uri.as_str(), text).unwrap());
        assert!(full.hit);
        assert!(!full.document.tokens.is_empty());
        fs::write(
            &source,
            "[proc4glsource]\n===\nPROCEDURE main() = { RETURN 123; }",
        )
        .unwrap();
        assert!(!cache.parse_file(&source).unwrap().hit);
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    #[test]
    fn incompatible_cache_rebuilds_without_changing_source() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("constant.w4gl");
        let text = "[constsource]\ndatatype = \"integer\"\ndefaultstring = \"42\"";
        fs::write(&file, text).unwrap();
        let cache = Cache::new(directory.path().join("cache"));
        let initial = cache.parse_file(&file).unwrap();
        let path = cache.path(&initial.document.source.uri);
        let mut record: Record = postcard::from_bytes(&read_checked(&path).unwrap()).unwrap();
        record.schema = "incompatible-future-schema".into();
        write_checked(&path, &postcard::to_stdvec(&record).unwrap()).unwrap();
        let summary_path = path.with_extension("summary");
        let mut summary: Summary =
            postcard::from_bytes(&read_checked(&summary_path).unwrap()).unwrap();
        summary.schema = "incompatible-future-schema".into();
        write_checked(&summary_path, &postcard::to_stdvec(&summary).unwrap()).unwrap();
        assert!(!cache.parse_file(&file).unwrap().hit);
        assert!(cache.parse_file(&file).unwrap().hit);
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }
}
