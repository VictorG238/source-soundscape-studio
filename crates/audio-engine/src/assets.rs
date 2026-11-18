use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Default, Clone)]
pub struct Sources {
    pub roots: Vec<PathBuf>,
}
impl Sources {
    pub fn resolve(&self, wave: &str) -> Option<PathBuf> {
        match Resolver::new(self.roots.clone()).lookup(wave).ok()? {
            AudioAsset::Loose(path) => Some(path),
            AudioAsset::Packed { .. } => None,
        }
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(data) => Ok(Self {
                roots: data
                    .lines()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut data = String::new();
        for root in &self.roots {
            let s = root
                .to_str()
                .filter(|s| !s.contains(['\n', '\r']))
                .ok_or_else(|| io::Error::other("Unsupported sound folder name"))?;
            data.push_str(s);
            data.push('\n');
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, data)?;
        fs::rename(tmp, path)
    }
}

mod catalog;
mod vpk;
pub use catalog::{CatalogEntry, CatalogSource, audio_extension};

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Default)]
pub struct Resolver(Arc<Vec<Mount>>);

struct Mount {
    path: PathBuf,
    index: Option<OnceLock<Result<vpk::Index, (io::ErrorKind, String)>>>,
}

#[derive(Debug)]
pub struct LookupError {
    pub reason: &'static str,
    pub searched: Vec<String>,
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)?;
        for location in &self.searched {
            write!(f, "\n{location}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LookupError {}

#[derive(Debug)]
pub(crate) enum AudioAsset {
    Loose(PathBuf),
    Packed {
        index: PathBuf,
        key: String,
        container: PathBuf,
        offset: u64,
        entry: vpk::Entry,
    },
}

impl AudioAsset {
    fn open(&self) -> io::Result<(Box<dyn symphonia::core::io::MediaSource>, &Path)> {
        match self {
            Self::Loose(path) => Ok((Box::new(fs::File::open(path)?), path)),
            Self::Packed {
                key,
                container,
                offset,
                entry,
                ..
            } => Ok((Box::new(entry.open(container, *offset)?), Path::new(key))),
        }
    }

    fn decode(self, budget: usize, cancel: impl Fn() -> bool) -> io::Result<crate::decode::Clip> {
        let (media, name) = self.open()?;
        crate::decode::decode_source(media, name, budget, cancel)
    }
    pub(crate) fn description(&self) -> String {
        match self {
            Self::Loose(path) => format!("Found in {}", path.display()),
            Self::Packed { index, key, .. } => format!("Found in VPK {} · {key}", index.display()),
        }
    }
}

pub fn wave_prefix(wave: &str) -> &str {
    let path = wave.trim_start_matches([
        '*', '#', '>', '<', '^', '@', ')', '}', '(', '$', '!', '?', '&', '~', '`', '+',
    ]);
    &wave[..wave.len() - path.len()]
}

fn relative_wave(wave: &str) -> Option<String> {
    let name = wave[wave_prefix(wave).len()..].replace('\\', "/");
    let name = if name
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("sound/"))
    {
        &name[6..]
    } else {
        &name
    };
    if name.is_empty()
        || name
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || name.contains([':', '\0'])
        || !Path::new(name)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
    {
        return None;
    }
    Some(name.to_owned())
}

pub fn case_path(path: &Path) -> io::Result<PathBuf> {
    match fs::metadata(path) {
        Ok(_) => return Ok(path.to_owned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let name = path.file_name().ok_or(io::ErrorKind::NotFound)?;
    let parent = path.parent().ok_or(io::ErrorKind::NotFound)?;
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        case_path(parent)?
    };
    let mut matched: Option<PathBuf> = None;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_name().eq_ignore_ascii_case(name) {
            if matched.is_some() {
                return Err(io::Error::other("Multiple paths differ only by case"));
            }
            matched = Some(entry.path());
        }
    }
    matched.ok_or_else(|| io::ErrorKind::NotFound.into())
}

pub fn is_vpk(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vpk"))
}

pub fn vpk_index_path(path: &Path) -> PathBuf {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return path.to_owned();
    };
    if stem.to_ascii_lowercase().ends_with("_dir") {
        return path.to_owned();
    }
    let prefix = stem
        .rsplit_once('_')
        .filter(|(_, suffix)| suffix.len() == 3 && suffix.bytes().all(|b| b.is_ascii_digit()))
        .map_or(stem, |(prefix, _)| prefix);
    case_path(&path.with_file_name(format!("{prefix}_dir.vpk"))).unwrap_or_else(|_| path.to_owned())
}

impl Resolver {
    pub fn same_sources(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn decode_wave(
        &self,
        wave: &str,
        budget: usize,
        cancel: impl Fn() -> bool,
    ) -> io::Result<crate::decode::Clip> {
        self.lookup(wave)
            .map_err(io::Error::other)?
            .decode(budget, cancel)
    }

    pub fn decode_entry(
        &self,
        entry: &CatalogEntry,
        budget: usize,
        cancel: impl Fn() -> bool,
    ) -> io::Result<crate::decode::Clip> {
        self.lookup_entry(entry)?.decode(budget, cancel)
    }

    pub fn probe_entry(&self, entry: &CatalogEntry) -> io::Result<crate::decode::Metadata> {
        let asset = self.lookup_entry(entry)?;
        let (media, name) = asset.open()?;
        crate::decode::probe(media, name)
    }

    fn lookup_entry(&self, entry: &CatalogEntry) -> io::Result<AudioAsset> {
        let mount = self
            .0
            .get(entry.source)
            .ok_or_else(|| io::Error::other("Audio source changed"))?;
        Self::lookup_in(&entry.wave, std::slice::from_ref(mount)).map_err(io::Error::other)
    }

    pub fn wave_for_file(&self, file: &Path) -> Option<String> {
        let file = file.canonicalize().ok()?;
        self.0
            .iter()
            .filter(|mount| mount.index.is_none())
            .find_map(|mount| {
                let root = mount.path.canonicalize().ok()?;
                let relative = file.strip_prefix(root).ok()?.to_str()?.replace('\\', "/");
                let wave = relative_wave(&relative)?;
                match self.lookup(&wave).ok()? {
                    AudioAsset::Loose(path) if path.canonicalize().ok().as_ref() == Some(&file) => {
                        Some(wave)
                    }
                    _ => None,
                }
            })
    }

    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self(Arc::new(
            paths
                .into_iter()
                .map(|path| Mount {
                    index: is_vpk(&path).then(OnceLock::new),
                    path,
                })
                .collect(),
        ))
    }

    pub fn describe(&self, wave: &str) -> Result<String, LookupError> {
        self.lookup(wave).map(|asset| asset.description())
    }

    pub(crate) fn lookup(&self, wave: &str) -> Result<AudioAsset, LookupError> {
        Self::lookup_in(wave, &self.0)
    }

    fn lookup_in(wave: &str, mounts: &[Mount]) -> Result<AudioAsset, LookupError> {
        let mut error = LookupError {
            reason: "Sound file not found",
            searched: Vec::new(),
        };
        let Some(name) = relative_wave(wave) else {
            error.reason = "Invalid relative Source sound path";
            return Err(error);
        };
        if mounts.is_empty() {
            error.reason = "No sound folders or VPK sources configured";
            return Err(error);
        }
        let key = format!("sound/{}", name.to_ascii_lowercase());
        for mount in mounts {
            let path = &mount.path;
            let (location, result) = if let Some(index) = &mount.index {
                let index = index.get_or_init(|| {
                    vpk::Index::open(path).map_err(|err| (err.kind(), err.to_string()))
                });
                let asset = match index {
                    Ok(index) => match index.entries.get(&key) {
                        Some(entry) => entry.location(path, index.data_start).and_then(
                            |(container, offset)| {
                                let container = case_path(&container).map_err(|err| {
                                    if err.kind() == io::ErrorKind::NotFound {
                                        io::Error::other(format!(
                                            "VPK chunk missing: {}",
                                            container.display()
                                        ))
                                    } else {
                                        err
                                    }
                                })?;
                                if entry.payload_len() > 0
                                    && offset + entry.payload_len()
                                        > fs::metadata(&container)?.len()
                                {
                                    return Err(io::Error::other(
                                        "VPK audio entry exceeds archive size",
                                    ));
                                }
                                Ok(AudioAsset::Packed {
                                    index: path.clone(),
                                    key: key.clone(),
                                    container,
                                    offset,
                                    entry: entry.clone(),
                                })
                            },
                        ),
                        None => Err(io::ErrorKind::NotFound.into()),
                    },
                    Err((kind, message)) => Err(io::Error::new(*kind, message.clone())),
                };
                (format!("{} · {key}", path.display()), asset)
            } else {
                let candidate = path.join(&name);
                let asset = case_path(&candidate).and_then(|found| {
                    if fs::metadata(&found)?.is_file() {
                        Ok(AudioAsset::Loose(found))
                    } else {
                        Err(io::ErrorKind::NotFound.into())
                    }
                });
                (candidate.display().to_string(), asset)
            };
            match result {
                Ok(asset) => return Ok(asset),
                Err(err) => {
                    match err.kind() {
                        io::ErrorKind::PermissionDenied => {
                            error.reason = "Permission denied while searching for sound"
                        }
                        io::ErrorKind::NotFound => {}
                        _ if error.reason == "Sound file not found" => {
                            error.reason = "Audio source could not be read"
                        }
                        _ => {}
                    }
                    error.searched.push(format!("{location} · {err}"));
                }
            }
        }
        Err(error)
    }
}
