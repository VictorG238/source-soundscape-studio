use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};
use symphonia::core::io::MediaSource;

const EMBEDDED: u16 = 0x7fff;
const MAX_TREE: usize = 64 * 1024 * 1024;
const MAX_AUDIO: u32 = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub archive: u16,
    offset: u32,
    length: u32,
    preload: Arc<[u8]>,
}

pub(super) struct Index {
    pub data_start: u64,
    pub entries: HashMap<String, Entry>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn word<'a>(tree: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let len = tree
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid("Unterminated VPK tree string"))?;
    let (value, tail) = tree.split_at(len);
    *tree = &tail[1..];
    Ok(value)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

impl Index {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        if u32_at(&header, 0) != 0x55aa1234 {
            return Err(invalid("Invalid VPK signature"));
        }
        let size = u32_at(&header, 8) as usize;
        let file_len = file.metadata()?.len();
        let (start, data_length) = match u32_at(&header, 4) {
            1 => (12, None),
            2 => {
                let mut extra = [0; 16];
                file.read_exact(&mut extra)?;
                (28, Some(u64::from(u32_at(&extra, 0))))
            }
            _ => return Err(invalid("Unsupported VPK version (expected 1 or 2)")),
        };
        let data_start = start + size as u64;
        if size > MAX_TREE || data_start > file_len {
            return Err(invalid("Invalid VPK directory size"));
        }
        let data_length = data_length.unwrap_or(file_len - data_start);
        if data_length > file_len - data_start {
            return Err(invalid("Truncated VPK data section"));
        }
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes)?;
        let mut tree = bytes.as_slice();
        let mut entries = HashMap::new();
        loop {
            let ext = word(&mut tree)?;
            if ext.is_empty() {
                break;
            }
            loop {
                let folder = word(&mut tree)?;
                if folder.is_empty() {
                    break;
                }
                loop {
                    let name = word(&mut tree)?;
                    if name.is_empty() {
                        break;
                    }
                    let (fields, tail) = tree
                        .split_at_checked(18)
                        .ok_or_else(|| invalid("Truncated VPK entry"))?;
                    let preload_len = usize::from(u16::from_le_bytes([fields[4], fields[5]]));
                    let archive = u16::from_le_bytes([fields[6], fields[7]]);
                    let offset = u32_at(fields, 8);
                    let length = u32_at(fields, 12);
                    if fields[16..18] != [255, 255] {
                        return Err(invalid("Invalid VPK entry terminator"));
                    }
                    let (preload, rest) = tail
                        .split_at_checked(preload_len)
                        .ok_or_else(|| invalid("Truncated VPK preload"))?;
                    tree = rest;
                    if !folder
                        .get(..6)
                        .is_some_and(|p| p.eq_ignore_ascii_case(b"sound/"))
                        && !folder.eq_ignore_ascii_case(b"sound")
                    {
                        continue;
                    }
                    if archive == EMBEDDED
                        && length > 0
                        && u64::from(offset) + u64::from(length) > data_length
                    {
                        return Err(invalid("VPK audio entry exceeds embedded data section"));
                    }
                    if let (Ok(folder), Ok(name), Ok(ext)) = (
                        std::str::from_utf8(folder),
                        std::str::from_utf8(name),
                        std::str::from_utf8(ext),
                    ) {
                        let key = format!("{folder}/{name}.{ext}").to_ascii_lowercase();
                        entries.entry(key).or_insert_with(|| Entry {
                            archive,
                            offset,
                            length,
                            preload: preload.into(),
                        });
                    }
                }
            }
        }
        Ok(Self {
            data_start,
            entries,
        })
    }
}

impl Entry {
    pub fn size(&self) -> u64 {
        u64::from(self.length) + self.preload.len() as u64
    }

    pub fn payload_len(&self) -> u64 {
        u64::from(self.length)
    }

    pub fn location(&self, directory: &Path, data_start: u64) -> io::Result<(PathBuf, u64)> {
        if self.archive == EMBEDDED || self.length == 0 {
            return Ok((directory.to_owned(), data_start + u64::from(self.offset)));
        }
        let stem = directory
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("Invalid VPK filename"))?;
        let prefix = stem
            .get(..stem.len().saturating_sub(4))
            .filter(|_| stem.to_ascii_lowercase().ends_with("_dir"))
            .ok_or_else(|| invalid("Split VPK requires a _dir.vpk index"))?;
        Ok((
            directory.with_file_name(format!("{prefix}_{:03}.vpk", self.archive)),
            u64::from(self.offset),
        ))
    }

    pub fn open(&self, path: &Path, offset: u64) -> io::Result<Payload> {
        if self.length > MAX_AUDIO {
            return Err(invalid("VPK audio payload exceeds preview size limit"));
        }
        let file = if self.length > 0 {
            let file = File::open(path)?;
            if offset + u64::from(self.length) > file.metadata()?.len() {
                return Err(invalid("VPK audio entry exceeds archive size"));
            }
            Some(file)
        } else {
            None
        };
        Ok(Payload {
            file,
            offset,
            size: self.size(),
            preload: self.preload.clone(),
            pos: 0,
        })
    }
}

pub(crate) struct Payload {
    file: Option<File>,
    offset: u64,
    size: u64,
    preload: Arc<[u8]>,
    pos: u64,
}

impl Read for Payload {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(self.size.saturating_sub(self.pos) as usize);
        if len == 0 {
            return Ok(0);
        }
        let preload = self.preload.len() as u64;
        let count = if self.pos < preload {
            let count = len.min((preload - self.pos) as usize);
            buf[..count].copy_from_slice(&self.preload[self.pos as usize..][..count]);
            count
        } else {
            let file = self
                .file
                .as_mut()
                .ok_or_else(|| invalid("Missing VPK payload"))?;
            file.seek(SeekFrom::Start(self.offset + self.pos - preload))?;
            file.read(&mut buf[..len])?
        };
        self.pos += count as u64;
        Ok(count)
    }
}

impl Seek for Payload {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(pos) => Some(pos),
            SeekFrom::End(delta) => self.size.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid VPK payload seek"))?;
        Ok(self.pos)
    }
}

impl MediaSource for Payload {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AudioAsset, Resolver, vpk_index_path};
    use std::{fs, io::Cursor};

    #[test]
    fn resolves_and_decodes_embedded_and_split_audio_with_preload() {
        let root = std::env::temp_dir().join(format!("sss-vpk-{}", std::process::id()));
        fs::create_dir_all(root.join("loose/Ambient")).unwrap();
        let loose = root.join("loose/Ambient/Bed.WAV");
        let wav = include_bytes!("../../../../tests/fixtures/audio/sound/studio/bed.wav");
        fs::write(&loose, wav).unwrap();
        for (version, archive) in [(1u32, EMBEDDED), (2, 0)] {
            let index = root.join(format!("test{version}_dir.vpk"));
            let chunk = root.join(format!("test{version}_000.vpk"));
            let mut tree = b"wav\0sound/ambient\0bed\0".to_vec();
            tree.extend(0u32.to_le_bytes());
            tree.extend(12u16.to_le_bytes());
            tree.extend(archive.to_le_bytes());
            tree.extend(0u32.to_le_bytes());
            tree.extend(((wav.len() - 12) as u32).to_le_bytes());
            tree.extend([255, 255]);
            tree.extend(&wav[..12]);
            tree.extend([0, 0, 0]);
            let mut bytes = 0x55aa1234u32.to_le_bytes().to_vec();
            bytes.extend(version.to_le_bytes());
            bytes.extend((tree.len() as u32).to_le_bytes());
            if version == 2 {
                bytes.extend([0; 16]);
            }
            bytes.extend(tree);
            if archive == EMBEDDED {
                bytes.extend(&wav[12..]);
            } else {
                fs::write(&chunk, &wav[12..]).unwrap();
            }
            fs::write(&index, bytes).unwrap();
            if archive != EMBEDDED {
                assert_eq!(vpk_index_path(&chunk), index);
            }
            let wave = "*#SOUND\\Ambient\\BED.wav";
            let resolver = Resolver::new(vec![index.clone(), root.join("loose")]);
            let mut catalog = Vec::new();
            assert!(
                resolver
                    .scan_catalog(
                        || false,
                        |entry| {
                            catalog.push(entry);
                            true
                        }
                    )
                    .is_empty()
            );
            assert_eq!(catalog.len(), 2);
            assert_eq!(catalog[0].wave, "ambient/bed.wav");
            assert_eq!(catalog[0].size, wav.len() as u64);
            assert!(catalog[0].effective && catalog[0].available && !catalog[1].effective);
            assert!(
                resolver
                    .probe_entry(&catalog[0])
                    .unwrap()
                    .duration
                    .is_some()
            );
            let embedded_clip = resolver
                .decode_entry(&catalog[0], 200_000, || false)
                .unwrap();
            assert_eq!(embedded_clip.frames.len(), 11025);
            let AudioAsset::Packed {
                container,
                offset,
                entry,
                ..
            } = resolver.lookup(wave).unwrap()
            else {
                panic!("VPK should take precedence");
            };
            let mut data = Vec::new();
            entry
                .open(&container, offset)
                .unwrap()
                .read_to_end(&mut data)
                .unwrap();
            assert_eq!(data.as_slice(), wav);
            let clip = crate::decode::decode_source(
                Box::new(Cursor::new(data)),
                Path::new(wave),
                200_000,
                || false,
            )
            .unwrap();
            assert_eq!(clip.frames.len(), 11025);
            assert!(clip.info.wave.iter().any(|value| *value > 0.3));
            let resolver = Resolver::new(vec![root.join("loose"), index.clone()]);
            assert!(matches!(
                resolver.lookup(wave).unwrap(),
                AudioAsset::Loose(_)
            ));
            assert!(resolver.describe("missing.wav").unwrap_err().searched.len() == 2);
            if archive != EMBEDDED {
                fs::remove_file(&chunk).unwrap();
                let err = Resolver::new(vec![index.clone()])
                    .describe(wave)
                    .unwrap_err();
                assert!(err.to_string().contains("VPK chunk missing"));
                let resolver = Resolver::new(vec![index.clone(), root.join("loose")]);
                let mut entries = Vec::new();
                resolver.scan_catalog(
                    || false,
                    |entry| {
                        entries.push(entry);
                        true
                    },
                );
                assert!(!entries[0].available && entries[1].effective);
            }
            fs::write(&index, [0u8; 12]).unwrap();
            assert!(Index::open(&index).is_err());
            fs::remove_file(index).unwrap();
        }
        fs::remove_file(loose).unwrap();
        fs::remove_dir(root.join("loose/Ambient")).unwrap();
        fs::remove_dir(root.join("loose")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
