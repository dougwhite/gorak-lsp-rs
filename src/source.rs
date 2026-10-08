//! Byte spans internally; UTF-16 conversion happens only at editor boundaries.
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }
    pub fn contains(self, byte: u32) -> bool {
        self.start <= byte && byte < self.end
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WideCharacter {
    start: u32,
    end: u32,
    loss: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub uri: Arc<str>,
    text: Arc<str>,
    lines: Arc<[u32]>,
    endings: Arc<[u8]>,
    wide: Arc<[WideCharacter]>,
    length: u32,
    pub fingerprint: [u8; 32],
}
impl Source {
    pub fn new(uri: impl Into<Arc<str>>, text: impl Into<Arc<str>>) -> Result<Self, &'static str> {
        let uri = uri.into();
        let uri: Arc<str> = canonical_uri(&uri).into();
        let text = text.into();
        if text.len() > u32::MAX as usize {
            return Err("Source exceeds the 4 GiB span address space");
        }
        let mut lines = vec![0];
        let mut endings = Vec::new();
        let bytes = text.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
                endings.push(if b == b'\n' && i > 0 && bytes[i - 1] == b'\r' {
                    2
                } else {
                    1
                });
                lines.push((i + 1) as u32);
            }
        }
        endings.push(0);
        let mut loss = 0;
        let wide = text
            .char_indices()
            .filter_map(|(start, ch)| {
                if ch.is_ascii() {
                    return None;
                }
                loss += (ch.len_utf8() - ch.len_utf16()) as u32;
                Some(WideCharacter {
                    start: start as u32,
                    end: (start + ch.len_utf8()) as u32,
                    loss,
                })
            })
            .collect::<Vec<_>>();
        Ok(Self {
            length: text.len() as u32,
            fingerprint: *blake3::hash(text.as_bytes()).as_bytes(),
            endings: endings.into(),
            wide: wide.into(),
            uri,
            text,
            lines: lines.into(),
        })
    }
    pub fn text(&self) -> &str {
        assert!(
            self.is_resident(),
            "source text must be loaded before text analysis"
        );
        &self.text
    }
    pub fn slice(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end as usize]
    }
    pub fn is_resident(&self) -> bool {
        self.text.len() == self.length as usize
    }
    pub fn resident_bytes(&self) -> usize {
        self.text.len()
    }
    pub fn release_text(&mut self) {
        self.text = Arc::from("");
    }
    fn floor_boundary(&self, byte: u32) -> u32 {
        let byte = byte.min(self.length);
        let i = self.wide.partition_point(|c| c.end <= byte);
        self.wide
            .get(i)
            .filter(|c| c.start < byte)
            .map_or(byte, |c| c.start)
    }
    fn units(&self, byte: u32) -> u32 {
        let i = self.wide.partition_point(|c| c.end <= byte);
        byte - i.checked_sub(1).map_or(0, |i| self.wide[i].loss)
    }
    pub fn position(&self, byte: u32) -> Position {
        let byte = self.floor_boundary(byte);
        let line = self.lines.partition_point(|&start| start <= byte) - 1;
        Position {
            line: line as u32,
            character: self.units(byte) - self.units(self.lines[line]),
        }
    }
    pub fn offset(&self, position: Position) -> u32 {
        let line = position.line as usize;
        let Some(&start) = self.lines.get(line) else {
            return self.length;
        };
        let end = self.lines.get(line + 1).copied().unwrap_or(self.length)
            - u32::from(self.endings[line]);
        let units = self
            .units(start)
            .saturating_add(position.character)
            .min(self.units(end));
        let i = self.wide.partition_point(|c| c.end - c.loss <= units);
        let byte = units + i.checked_sub(1).map_or(0, |i| self.wide[i].loss);
        self.floor_boundary(byte).clamp(start, end)
    }
    pub fn range(&self, span: Span) -> Range {
        Range {
            start: self.position(span.start),
            end: self.position(span.end),
        }
    }
    pub fn replace(&self, range: Range, replacement: &str) -> Result<Self, &'static str> {
        let start = self.offset(range.start) as usize;
        let end = self.offset(range.end) as usize;
        if start > end {
            return Err("Reversed edit range");
        }
        let mut text = String::with_capacity(self.text.len() + replacement.len());
        text.push_str(&self.text[..start]);
        text.push_str(replacement);
        text.push_str(&self.text[end..]);
        Self::new(self.uri.clone(), text)
    }
}
/// Decode URI paths independently of the host OS, including in-memory test sources.
pub fn uri_path(uri: &str) -> Option<std::path::PathBuf> {
    let uri = url::Url::parse(uri).ok()?;
    uri.to_file_path().ok().or_else(|| {
        (uri.scheme() == "file" && uri.host_str().is_none()).then(|| {
            std::path::PathBuf::from(
                percent_encoding::percent_decode_str(uri.path())
                    .decode_utf8_lossy()
                    .as_ref(),
            )
        })
    })
}

/// Match editor and filesystem URI spellings on Windows without changing displayed names.
pub fn canonical_uri(uri: &str) -> String {
    if !cfg!(windows) {
        return uri.to_owned();
    }
    let Ok(url) = url::Url::parse(uri) else {
        return uri.to_owned();
    };
    let Ok(path) = url.to_file_path() else {
        return uri.to_owned();
    };
    // Deleted files still need the same parent spelling as their indexed URI.
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    let path = loop {
        if let Ok(mut canonical) = std::fs::canonicalize(ancestor) {
            for part in missing.iter().rev() {
                canonical.push(part);
            }
            break canonical;
        }
        let (Some(parent), Some(name)) = (ancestor.parent(), ancestor.file_name()) else {
            break path.clone();
        };
        missing.push(name);
        ancestor = parent;
    };
    let Ok(mut url) = url::Url::from_file_path(path) else {
        return uri.to_owned();
    };
    let path = url.path();
    if path.as_bytes().get(2) == Some(&b':') {
        let mut path = path.to_owned();
        path.replace_range(1..2, &path[1..2].to_ascii_lowercase());
        url.set_path(&path);
    }
    url.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_crlf_and_non_bmp() {
        let s = Source::new("file:///test", "a🎈b\r\nnext\rfinal").unwrap();
        for byte in [0, 1, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17] {
            if s.text().is_char_boundary(byte) && ![6, 7, 12].contains(&byte) {
                assert_eq!(s.offset(s.position(byte as u32)), byte as u32);
            }
        }
        assert_eq!(
            s.position(5),
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(
            s.offset(Position {
                line: 0,
                character: 2
            }),
            1
        );
    }
    #[cfg(windows)]
    #[test]
    fn deleted_file_keeps_canonical_parent_spelling() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("MixedCaseApplication");
        std::fs::create_dir(&app).unwrap();
        let path = app.join("panel.wml");
        std::fs::write(&path, "<frame/>").unwrap();
        let uri = url::Url::from_file_path(&path).unwrap();
        let indexed = canonical_uri(uri.as_str());
        std::fs::remove_file(&path).unwrap();
        let watcher_path =
            std::path::PathBuf::from(app.to_string_lossy().to_uppercase()).join("panel.wml");
        let watcher = url::Url::from_file_path(watcher_path).unwrap();
        assert_eq!(canonical_uri(watcher.as_str()), indexed);
        assert_eq!(canonical_uri(uri.as_str()), indexed);
    }
    #[test]
    fn edits_use_utf16_not_bytes() {
        let s = Source::new("test", "x🎈y").unwrap();
        let changed = s
            .replace(
                Range {
                    start: Position {
                        line: 0,
                        character: 1,
                    },
                    end: Position {
                        line: 0,
                        character: 3,
                    },
                },
                "ok",
            )
            .unwrap();
        assert_eq!(changed.text(), "xoky");
    }
}
