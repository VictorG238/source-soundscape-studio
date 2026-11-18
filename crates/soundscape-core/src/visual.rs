use crate::{
    Script,
    keyvalues::{Entry, Value, token},
};
use std::{borrow::Cow, io, ops::Range};

impl Entry {
    pub fn span(&self) -> Range<usize> {
        self.key.start..self.end
    }
    pub fn children(&self) -> &[Entry] {
        match &self.value {
            Value::Block(items) => items,
            _ => &[],
        }
    }
    pub fn name<'a>(&self, doc: &'a Script) -> Cow<'a, str> {
        token(doc.bytes(), self.key.clone())
    }
    pub fn text<'a>(&self, doc: &'a Script) -> Option<Cow<'a, str>> {
        match &self.value {
            Value::Text(r) => Some(token(doc.bytes(), r.clone())),
            _ => None,
        }
    }
    pub fn named<'a>(&'a self, doc: &'a Script, key: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.children()
            .iter()
            .filter(move |e| e.name(doc).eq_ignore_ascii_case(key))
    }
}

impl Script {
    pub fn entry_at(&self, path: &[usize]) -> io::Result<&Entry> {
        let mut entries = self.tree.entries.as_slice();
        let mut result = None;
        for i in path {
            let e = entries
                .get(*i)
                .ok_or_else(|| io::Error::other("Selection no longer exists"))?;
            result = Some(e);
            entries = e.children();
        }
        result.ok_or_else(|| io::Error::other("Select a block"))
    }

    fn insert_fields(&mut self, path: &[usize], fields: &str) -> io::Result<()> {
        let block = self.entry_at(path)?;
        if !matches!(block.value, Value::Block(_)) {
            return Err(io::Error::other("Expected a block"));
        }
        let at = block.end - 1;
        let newline = if self.bytes.contains(&b'\r') {
            "\r\n"
        } else {
            "\n"
        };
        let indent = "\t".repeat(path.len());
        let text = format!(
            "{newline}{indent}{}{newline}{}",
            fields.replace('\n', &format!("{newline}{indent}")),
            "\t".repeat(path.len() - 1)
        );
        self.change(at..at, text.into_bytes())
    }

    pub fn set_property(&mut self, path: &[usize], key: &str, value: &str) -> io::Result<()> {
        self.validate_text(value)?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(io::Error::other("Invalid property key"));
        }
        let parent = self.entry_at(path)?;
        let mut found = parent.named(self, key);
        let range = match (found.next(), found.next()) {
            (None, None) => None,
            (
                Some(Entry {
                    value: Value::Text(r),
                    ..
                }),
                None,
            ) => Some(r.clone()),
            _ => {
                return Err(io::Error::other(
                    "Repeated or structured property preserved in Source",
                ));
            }
        };
        drop(found);
        if let Some(r) = range {
            self.edit_token(r, value)
        } else {
            self.insert_fields(path, &format!("{key} \"{value}\""))
        }
    }

    pub fn add_variant(&mut self, path: &[usize], wave: &str) -> io::Result<()> {
        self.add_variants(path, &[wave])
    }

    pub fn add_variants(&mut self, path: &[usize], waves: &[impl AsRef<str>]) -> io::Result<()> {
        if waves.is_empty() {
            return Ok(());
        }
        let mut lines = Vec::with_capacity(waves.len());
        for wave in waves {
            let wave = wave.as_ref();
            self.validate_text(wave)?;
            lines.push(format!("wave \"{wave}\""));
        }
        let parent = self.entry_at(path)?;
        if !parent.name(self).eq_ignore_ascii_case("playrandom") {
            return Err(io::Error::other("Select a random layer"));
        }
        let mut groups = parent
            .children()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name(self).eq_ignore_ascii_case("rndwave"));
        let group = match (groups.next(), groups.next()) {
            (None, None) => None,
            (
                Some((
                    i,
                    Entry {
                        value: Value::Block(_),
                        ..
                    },
                )),
                None,
            ) => Some(i),
            _ => return Err(io::Error::other("Ambiguous rndwave preserved in Source")),
        };
        if let Some(i) = group {
            let mut target = path.to_vec();
            target.push(i);
            self.insert_fields(&target, &lines.join("\n"))
        } else {
            self.insert_fields(path, &format!("rndwave\n{{\n\t{}\n}}", lines.join("\n\t")))
        }
    }

    pub fn copy_soundscape(&mut self, i: usize) -> io::Result<()> {
        let e = self.entry_at(&[i])?;
        let name = e.name(self);
        let mut n = 1;
        let copy = loop {
            let candidate = format!("{name}.copy{n}");
            if !self
                .tree
                .entries
                .iter()
                .any(|e| e.name(self).eq_ignore_ascii_case(&candidate))
            {
                break candidate;
            }
            n += 1;
        };
        self.validate_text(&copy)?;
        let mut data = if self.bytes.contains(&b'\r') {
            b"\r\n".to_vec()
        } else {
            vec![b'\n']
        };
        data.extend(format!("\"{copy}\"").bytes());
        data.extend_from_slice(&self.bytes[e.key.end..e.end]);
        self.change(e.end..e.end, data)
    }
}
