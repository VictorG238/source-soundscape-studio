use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
pub mod keyvalues;
mod visual;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINES: usize = 250_000;

pub struct Script {
    pub tree: keyvalues::Tree,
    pub path: PathBuf,
    bytes: Vec<u8>,
    lines: Vec<usize>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    saved: Vec<u8>,
    dirty: bool,
    revision: u64,
}

struct Change {
    start: usize,
    before: Vec<u8>,
    after: Vec<u8>,
}

impl Script {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut script = Self::from_bytes(PathBuf::new(), b"\"new.soundscape\"\n{\n}\n".to_vec())?;
        script.save(path)?;
        Ok(script)
    }
    pub fn create_manifest(path: &Path) -> io::Result<()> {
        let mut doc = Self::from_bytes(
            PathBuf::new(),
            b"soundscaples_manifest
{
}
"
            .to_vec(),
        )?;
        doc.save(path)
    }

    pub fn register_manifest_script(&mut self, reference: &str) -> io::Result<()> {
        self.validate_text(reference)?;
        let path = reference.replace('\\', "/");
        let lower = path.to_ascii_lowercase();
        if !lower.starts_with("scripts/")
            || !lower.ends_with(".txt")
            || path
                .split('/')
                .any(|part| matches!(part, "" | "." | "..") || part.contains(':'))
            || lower == "scripts/soundscapes_manifest.txt"
        {
            return Err(io::Error::other(
                "Expected a game-relative script path under scripts/",
            ));
        }
        let block = self
            .tree
            .entries
            .first()
            .filter(|entry| matches!(entry.value, keyvalues::Value::Block(_)))
            .ok_or_else(|| io::Error::other("Expected a manifest containing a root block"))?;
        if block
            .named(self, "file")
            .filter_map(|entry| entry.text(self))
            .any(|existing| existing.replace('\\', "/").eq_ignore_ascii_case(&path))
        {
            return Ok(());
        }
        let at = block.end - 1;
        let nl = if self.bytes.windows(2).any(|s| s == b"\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        self.change(
            at..at,
            format!("{nl}\t\"file\" \"{path}\"{nl}").into_bytes(),
        )
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let file = File::open(&path)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Self::from_bytes(path, bytes)
    }

    fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> io::Result<Self> {
        if bytes.len() > MAX_BYTES || bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected a text script of at most 8 MiB without NUL bytes",
            ));
        }
        let mut lines = vec![0];
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' && i + 1 < bytes.len() {
                if lines.len() >= MAX_LINES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Script exceeds 250,000 lines",
                    ));
                }
                lines.push(i + 1);
            }
        }
        let tree = keyvalues::Tree::parse(&bytes);
        Ok(Self {
            path,
            saved: bytes.clone(),
            dirty: false,
            revision: 0,
            bytes,
            lines,
            tree,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn modified(&self) -> bool {
        self.dirty
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        replacement: &[u8],
        validate: bool,
    ) -> io::Result<()> {
        if self.bytes.get(range.clone()).is_none()
            || self.bytes.len() - range.len() + replacement.len() > MAX_BYTES
        {
            return Err(io::Error::other("Edit exceeds the document bounds"));
        }
        let mut next = self.bytes.clone();
        next.splice(range, replacement.iter().copied());
        let parsed = Self::from_bytes(self.path.clone(), next)?;
        if validate && let Some(error) = &parsed.tree.error {
            let (line, col) = parsed.location(error.offset);
            return Err(io::Error::other(format!(
                "Line {line}, column {col} · {}",
                error.message
            )));
        }
        self.bytes = parsed.bytes;
        self.tree = parsed.tree;
        self.lines = parsed.lines;
        self.dirty = self.saved != self.bytes;
        self.revision += 1;
        Ok(())
    }

    /// Return a reversible text view. Legacy files use a one-to-one Latin-1 byte mapping.
    pub fn source_text(&self) -> String {
        let bytes = self
            .bytes
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&self.bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => bytes.iter().copied().map(char::from).collect(),
        }
    }

    pub fn apply_source(&mut self, text: &str) -> io::Result<()> {
        let bom = self.bytes.starts_with(b"\xef\xbb\xbf");
        let offset = if bom { 3 } else { 0 };
        let before = &self.bytes[offset..];
        let after = if std::str::from_utf8(before).is_ok() {
            text.as_bytes().to_vec()
        } else {
            text.chars()
                .map(|ch| {
                    u8::try_from(u32::from(ch)).map_err(|_| io::Error::other(
                    "This legacy script can only represent characters in the Latin-1 byte range"
                ))
                })
                .collect::<io::Result<Vec<_>>>()?
        };
        if before == after {
            return Ok(());
        }
        let prefix = before
            .iter()
            .zip(&after)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = before[prefix..]
            .iter()
            .rev()
            .zip(after[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.change(
            offset + prefix..self.bytes.len() - suffix,
            after[prefix..after.len() - suffix].to_vec(),
        )
    }

    pub fn edit_token(&mut self, range: Range<usize>, text: &str) -> io::Result<()> {
        self.validate_text(text)?;
        self.change(range, format!("\"{text}\"").into_bytes())
    }

    fn validate_text(&self, text: &str) -> io::Result<()> {
        if text.contains(['"', '\n', '\r', '\0']) || text.ends_with('\\') {
            return Err(io::Error::other(
                "Value must be one line without quotes or a trailing backslash",
            ));
        }
        if !text.is_ascii() && std::str::from_utf8(&self.bytes).is_err() {
            return Err(io::Error::other("Use ASCII for edits to legacy scripts"));
        }
        Ok(())
    }

    pub fn add_layer(&mut self, block: &Range<usize>, layer: &str) -> io::Result<()> {
        let entry = self
            .tree
            .entries
            .iter()
            .find(|e| &e.key == block && matches!(e.value, keyvalues::Value::Block(_)))
            .ok_or_else(|| io::Error::other("Select a soundscape block"))?;
        let at = entry.end - 1;
        let fields = match layer {
            "playlooping" => "\t\tvolume \"1\"\n\t\twave \"\"",
            "playrandom" => "\t\ttime \"1,5\"\n\t\trndwave\n\t\t{\n\t\t\twave \"\"\n\t\t}",
            "playsoundscape" => "\t\tname \"\"",
            _ => return Err(io::Error::other("Unknown layer type")),
        };
        let mut text = format!("\n\t{layer}\n\t{{\n{fields}\n\t}}\n");
        if self.bytes.windows(2).any(|s| s == b"\r\n") {
            text = text.replace('\n', "\r\n");
        }
        self.change(at..at, text.into_bytes())
    }

    fn change(&mut self, range: Range<usize>, after: Vec<u8>) -> io::Result<()> {
        let before = self
            .bytes
            .get(range.clone())
            .ok_or_else(|| io::Error::other("Invalid selection"))?
            .to_vec();
        let change = Change {
            start: range.start,
            before,
            after,
        };
        self.replace(range, &change.after, true)?;
        self.redo.clear();
        self.undo.push(change);
        Ok(())
    }

    pub fn add_soundscape(&mut self) -> io::Result<()> {
        let mut i = 1;
        let names: std::collections::HashSet<_> = self
            .tree
            .entries
            .iter()
            .map(|e| keyvalues::token(&self.bytes, e.key.clone()).to_ascii_lowercase())
            .collect();
        while names.contains(&format!("new.soundscape{i}")) {
            i += 1;
        }
        let end = self.bytes.len();
        let nl = if self.bytes.windows(2).any(|s| s == b"\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        self.change(
            end..end,
            format!("{nl}\"new.soundscape{i}\"{nl}{{{nl}}}{nl}").into_bytes(),
        )
    }

    pub fn duplicate_entry(&mut self, range: Range<usize>) -> io::Result<()> {
        let mut copy = if self.bytes.windows(2).any(|s| s == b"\r\n") {
            b"\r\n".to_vec()
        } else {
            vec![b'\n']
        };
        copy.extend_from_slice(
            self.bytes
                .get(range.clone())
                .ok_or_else(|| io::Error::other("Invalid selection"))?,
        );
        self.change(range.end..range.end, copy)
    }

    pub fn remove_entry(&mut self, range: Range<usize>) -> io::Result<()> {
        self.change(range, Vec::new())
    }

    pub fn swap_entries(&mut self, a: Range<usize>, b: Range<usize>) -> io::Result<()> {
        if a.end > b.start || b.end > self.bytes.len() || a.start > a.end || b.start > b.end {
            return Err(io::Error::other("Invalid selection"));
        }
        let mut text = self.bytes[b.clone()].to_vec();
        text.extend_from_slice(&self.bytes[a.end..b.start]);
        text.extend_from_slice(&self.bytes[a.clone()]);
        self.change(a.start..b.end, text)
    }

    pub fn history(&mut self, redo: bool) -> io::Result<()> {
        let stack = if redo { &mut self.redo } else { &mut self.undo };
        let Some(change) = stack.pop() else {
            return Ok(());
        };
        let (old, new) = if redo {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        let result = self.replace(change.start..change.start + old.len(), new, false);
        if redo == result.is_ok() {
            self.undo.push(change);
        } else {
            self.redo.push(change);
        }
        result
    }

    pub fn save(&mut self, dest: &Path) -> io::Result<()> {
        let dest = dest.to_path_buf();
        let same = dest.canonicalize().is_ok_and(|p| p == self.path);
        if same && std::fs::read(&dest)? != self.saved {
            return Err(io::Error::other(
                "File changed on disk. Save to a new path.",
            ));
        }
        if !same && dest.exists() {
            return Err(io::Error::other("Choose a new file name for Save As"));
        }
        let temp = dest.with_file_name(format!(".sss-{}.tmp", std::process::id()));
        let mut file = File::create_new(&temp)?;
        let result = (|| {
            file.write_all(&self.bytes)?;
            file.sync_all()?;
            drop(file);
            if same {
                std::fs::rename(&temp, &dest)?;
            } else {
                std::fs::hard_link(&temp, &dest)?;
            }
            Ok::<(), io::Error>(())
        })();
        if !same || result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result?;
        self.path = dest.canonicalize()?;
        self.saved.clone_from(&self.bytes);
        self.dirty = false;
        Ok(())
    }

    pub fn location(&self, offset: usize) -> (usize, usize) {
        let line = self.lines.partition_point(|start| *start <= offset);
        (line, offset - self.lines[line - 1] + 1)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn line(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let Some(start) = self.lines.get(i) else {
            return "".into();
        };
        let end = self.lines.get(i + 1).copied().unwrap_or(self.bytes.len());
        let line = &self.bytes[*start..end];
        let line = if i == 0 {
            line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(line)
        } else {
            line
        };
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_undo_redo_and_save_preserve_surrounding_bytes() {
        let dir = std::env::temp_dir().join(format!("sss-document-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("coast.txt");
        let initial = b"\xef\xbb\xbf// legacy \xff\r\ncoast { volume \"1\" }\r\n";
        std::fs::write(&path, initial).unwrap();
        let mut script = Script::open(&path).unwrap();
        script.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), initial);
        let at = initial.iter().position(|b| *b == b'"').unwrap();
        script.edit_token(at..at + 3, "0.5").unwrap();
        let changed = script.bytes().to_vec();
        assert_eq!(&changed[..at], &initial[..at]);
        assert_eq!(&changed[at + 5..], &initial[at + 3..]);
        script.history(false).unwrap();
        assert!(!script.modified());
        script.history(true).unwrap();
        assert_eq!(script.bytes(), changed);
        let block = script.tree.entries[0].key.clone();
        script.add_layer(&block, "playrandom").unwrap();
        script.save(&path).unwrap();
        assert_eq!(Script::open(&path).unwrap().bytes(), script.bytes());
        std::fs::write(&path, b"external change").unwrap();
        assert!(script.save(&path).is_err());
        assert!(Script::create(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external change");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn retains_original_bytes_and_indexes_lines() {
        let bytes = b"\xef\xbb\xbf// coast\r\n\xff\r\n";
        let script = Script::from_bytes(PathBuf::new(), bytes.to_vec()).unwrap();
        assert_eq!(script.bytes(), bytes);
        assert_eq!(script.line_count(), 2);
        assert_eq!(script.line(0), "// coast");
        assert_eq!(script.line(1), "�");
    }
    #[test]
    fn accepts_empty_text_and_rejects_binary_and_excess_lines() {
        assert_eq!(
            Script::from_bytes(PathBuf::new(), vec![]).unwrap().line(0),
            ""
        );
        for bytes in [
            vec![0],
            vec![b'\n'; MAX_LINES + 1],
            vec![b'x'; MAX_BYTES + 1],
        ] {
            assert!(Script::from_bytes(PathBuf::new(), bytes).is_err());
        }
    }

    #[test]
    fn source_draft_round_trips_with_visual_history_and_legacy_bytes() {
        let original = b"\xef\xbb\xbf// legacy \xe9\r\nscene { playlooping { wave first.wav } extension x extension y }\r\n";
        let mut doc = Script::from_bytes(PathBuf::new(), original.to_vec()).unwrap();
        let invalid = doc.source_text() + "incomplete {";
        assert!(
            doc.apply_source(&invalid)
                .unwrap_err()
                .to_string()
                .contains("Line 3")
        );
        assert_eq!(doc.bytes(), original);
        assert!(!doc.can_undo());
        let edited = doc.source_text().replace("first.wav", "second.wav")
            + "manual {\r\n\tplaylooping { wave bed.wav }\r\n}\r\n";
        doc.apply_source(&edited).unwrap();
        assert_eq!(doc.source_text(), edited);
        assert_eq!(doc.tree.entries[1].name(&doc), "manual");
        assert_eq!(doc.tree.entries[0].named(&doc, "extension").count(), 2);
        let layer = &doc.tree.entries[0].children()[0];
        let value = &layer.children()[0].value;
        let keyvalues::Value::Text(span) = value else {
            panic!("Expected wave")
        };
        doc.edit_token(span.clone(), "third.wav").unwrap();
        assert!(doc.source_text().contains("third.wav"));
        doc.history(false).unwrap();
        assert_eq!(doc.source_text(), edited);
        doc.history(false).unwrap();
        assert_eq!(doc.bytes(), original);
        doc.history(true).unwrap();
        assert_eq!(doc.source_text(), edited);
    }

    #[test]
    fn manifest_registration_is_lossless_and_undoable() {
        let original = b"soundscaples_manifest\r\n{\r\n\tfile scripts/base.txt\r\n\t// preserve this\r\n\tunknown keep\r\n}\r\n";
        let mut doc = Script::from_bytes(PathBuf::new(), original.to_vec()).unwrap();
        doc.register_manifest_script("scripts/custom.txt").unwrap();
        let registered = doc.bytes().to_vec();
        doc.register_manifest_script("SCRIPTS\\CUSTOM.TXT").unwrap();
        assert_eq!(doc.bytes(), registered);
        assert!(
            doc.source_text()
                .contains("\t// preserve this\r\n\tunknown keep")
        );
        let files: Vec<_> = doc.tree.entries[0]
            .named(&doc, "file")
            .map(|entry| entry.span())
            .collect();
        doc.swap_entries(files[0].clone(), files[1].clone())
            .unwrap();
        let first = doc.tree.entries[0]
            .named(&doc, "file")
            .next()
            .unwrap()
            .span();
        doc.remove_entry(first).unwrap();
        assert!(!doc.source_text().contains("custom.txt"));
        for _ in 0..3 {
            doc.history(false).unwrap();
        }
        assert_eq!(doc.bytes(), original);
    }
}
