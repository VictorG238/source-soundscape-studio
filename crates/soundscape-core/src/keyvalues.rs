use std::ops::Range;

#[derive(Debug)]
pub struct Entry {
    pub end: usize,
    pub key: Range<usize>,
    pub value: Value,
}

#[derive(Debug)]
pub enum Value {
    Text(Range<usize>),
    Block(Vec<Entry>),
}

#[derive(Debug)]
pub struct Diagnostic {
    pub offset: usize,
    pub message: &'static str,
}

#[derive(Default)]
pub struct Tree {
    pub entries: Vec<Entry>,
    pub error: Option<Diagnostic>,
}

struct Reader<'a> {
    src: &'a [u8],
    at: usize,
    count: usize,
}

impl Reader<'_> {
    fn error(&self, message: &'static str) -> Diagnostic {
        Diagnostic {
            offset: self.at,
            message,
        }
    }

    fn peek(&mut self) -> Option<u8> {
        loop {
            while self.src.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
            if !self.src[self.at..].starts_with(b"//") {
                return self.src.get(self.at).copied();
            }
            self.at += self.src[self.at..]
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(self.src.len() - self.at);
        }
    }

    fn word(&mut self) -> Result<Range<usize>, Diagnostic> {
        let next = self.peek();
        let start = self.at;
        match next {
            None | Some(b'{' | b'}') => Err(self.error("Expected a key or value")),
            Some(b'"') => {
                self.at += 1;
                while let Some(&b) = self.src.get(self.at) {
                    self.at += 1;
                    if b == b'"' {
                        return Ok(start..self.at);
                    }
                    if b == b'\\' && matches!(self.src.get(self.at), Some(b'"' | b'\\')) {
                        self.at += 1;
                    }
                }
                Err(self.error("Unterminated quoted string"))
            }
            Some(_) => {
                while self
                    .src
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'{' | b'}' | b'"'))
                {
                    self.at += 1;
                }
                Ok(start..self.at)
            }
        }
    }

    fn entry(&mut self, depth: usize) -> Result<Entry, Diagnostic> {
        self.count += 1;
        if self.count > 100_000 || depth > 64 {
            return Err(self.error("KeyValues structure exceeds the editor limit"));
        }
        let key = self.word()?;
        let value = if self.peek() == Some(b'{') {
            self.at += 1;
            let mut children = Vec::new();
            loop {
                match self.peek() {
                    Some(b'}') => {
                        self.at += 1;
                        break;
                    }
                    None => return Err(self.error("Expected a closing brace")),
                    _ => children.push(self.entry(depth + 1)?),
                }
            }
            Value::Block(children)
        } else {
            Value::Text(self.word()?)
        };
        Ok(Entry {
            key,
            value,
            end: self.at,
        })
    }
}

impl Tree {
    pub fn parse(src: &[u8]) -> Self {
        let mut reader = Reader {
            src,
            at: if src.starts_with(b"\xef\xbb\xbf") {
                3
            } else {
                0
            },
            count: 0,
        };
        let mut tree = Self::default();
        while reader.peek().is_some() {
            match reader.entry(0) {
                Ok(entry) => tree.entries.push(entry),
                Err(error) => {
                    tree.error = Some(error);
                    break;
                }
            }
        }
        tree
    }
}

pub fn token(bytes: &[u8], range: Range<usize>) -> std::borrow::Cow<'_, str> {
    let raw = &bytes[range];
    let raw = raw
        .strip_prefix(b"\"")
        .and_then(|s| s.strip_suffix(b"\""))
        .unwrap_or(raw);
    String::from_utf8_lossy(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_duplicates_comments_unknown_blocks() {
        let src = b"\xef\xbb\xbf\"a\" { // x\r\n wave \"x\" wave y other { p q } }";
        let tree = Tree::parse(src);
        assert!(tree.error.is_none());
        let Value::Block(children) = &tree.entries[0].value else {
            panic!()
        };
        assert_eq!(
            children
                .iter()
                .map(|e| token(src, e.key.clone()))
                .collect::<Vec<_>>(),
            ["wave", "wave", "other"]
        );
    }
    #[test]
    fn malformed_suffix_preserves_completed_entries() {
        for tail in ["}", "bad {", "bad \"", "bad { x }"] {
            let tree = Tree::parse(format!("valid {{}} {tail}").as_bytes());
            assert_eq!(tree.entries.len(), 1);
            assert!(tree.error.is_some());
        }
        assert!(Tree::parse("x {".repeat(70).as_bytes()).error.is_some());
    }
}
