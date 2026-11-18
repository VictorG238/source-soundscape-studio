use super::{Resolver, case_path, relative_wave, vpk};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct CatalogSource {
    pub path: PathBuf,
    pub packed: bool,
}

#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub wave: String,
    pub source: usize,
    pub size: u64,
    pub available: bool,
    pub effective: bool,
}

pub fn audio_extension(path: &Path) -> Option<&str> {
    let ext = path.extension()?.to_str()?;
    ["wav", "mp3", "ogg", "flac"]
        .into_iter()
        .find(|candidate| ext.eq_ignore_ascii_case(candidate))
}

impl Resolver {
    pub fn catalog_sources(&self) -> Vec<CatalogSource> {
        self.0
            .iter()
            .map(|mount| CatalogSource {
                path: mount.path.clone(),
                packed: mount.index.is_some(),
            })
            .collect()
    }

    /// Enumerate in resolver order without reading audio payloads. Return false from
    /// the visitor to stop. Warnings are per source, rather than per inaccessible file.
    pub fn scan_catalog(
        &self,
        cancelled: impl Fn() -> bool,
        mut visit: impl FnMut(CatalogEntry) -> bool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut seen = HashSet::new();
        for (source, mount) in self.0.iter().enumerate() {
            if cancelled() {
                break;
            }
            let mut emit = |wave: String, size, available| {
                let effective = available && seen.insert(wave.to_ascii_lowercase());
                visit(CatalogEntry {
                    wave,
                    source,
                    size,
                    available,
                    effective,
                })
            };
            if let Some(index) = &mount.index {
                let index = index.get_or_init(|| {
                    vpk::Index::open(&mount.path).map_err(|err| (err.kind(), err.to_string()))
                });
                match index {
                    Ok(index) => {
                        let mut lengths = HashMap::new();
                        for (key, entry) in &index.entries {
                            if cancelled() {
                                return warnings;
                            }
                            let Some(wave) = key.strip_prefix("sound/") else {
                                continue;
                            };
                            if audio_extension(Path::new(wave)).is_none()
                                || relative_wave(wave).as_deref() != Some(wave)
                            {
                                continue;
                            }
                            let available = entry.payload_len() == 0
                                || entry.location(&mount.path, index.data_start).is_ok_and(
                                    |(container, offset)| {
                                        lengths
                                            .entry(container)
                                            .or_insert_with_key(|path| {
                                                fs::metadata(case_path(path).ok()?)
                                                    .ok()
                                                    .map(|m| m.len())
                                            })
                                            .is_some_and(|len| offset + entry.payload_len() <= len)
                                    },
                                );
                            if !emit(wave.to_owned(), entry.size(), available) {
                                return warnings;
                            }
                        }
                    }
                    Err((_, error)) => warnings.push(format!("{}: {error}", mount.path.display())),
                }
                continue;
            }

            let root = match case_path(&mount.path)
                .and_then(|path| fs::read_dir(&path).map(|dir| (path, dir)))
            {
                Ok(root) => root,
                Err(error) => {
                    warnings.push(format!("{}: {error}", mount.path.display()));
                    continue;
                }
            };
            let (base, dir) = root;
            let mut dirs = vec![dir];
            let mut skipped = false;
            while let Some(dir) = dirs.last_mut() {
                if cancelled() {
                    return warnings;
                }
                let Some(child) = dir.next() else {
                    dirs.pop();
                    continue;
                };
                let Ok(child) = child else {
                    skipped = true;
                    continue;
                };
                let Ok(kind) = child.file_type() else {
                    skipped = true;
                    continue;
                };
                let path = child.path();
                if kind.is_dir() && dirs.len() < 64 {
                    match fs::read_dir(&path) {
                        Ok(dir) => dirs.push(dir),
                        Err(_) => skipped = true,
                    }
                } else if kind.is_file() && audio_extension(&path).is_some() {
                    let Some(relative) = path.strip_prefix(&base).ok().and_then(Path::to_str)
                    else {
                        continue;
                    };
                    let wave = relative.replace('\\', "/");
                    if relative_wave(&wave).as_deref() != Some(wave.as_str())
                        || wave.contains(['"', '\r', '\n'])
                    {
                        continue;
                    }
                    let Ok(metadata) = child.metadata() else {
                        skipped = true;
                        continue;
                    };
                    if !emit(wave, metadata.len(), true) {
                        return warnings;
                    }
                }
            }
            if skipped {
                warnings.push(format!(
                    "Some files could not be read in {}",
                    mount.path.display()
                ));
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_loose_assets_in_mount_order_and_converts_source_paths() {
        let root = std::env::temp_dir().join(format!("sss-catalog-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let custom = root.join("mod/sound");
        let base = root.join("base/sound");
        let path = custom.join("Ambient/Wind.wav");
        let wav = include_bytes!("../../../../tests/fixtures/audio/sound/studio/bed.wav");
        for file in [
            &path,
            &base.join("ambient/wind.wav"),
            &base.join("ambient/wind_other.wav"),
        ] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, wav).unwrap();
        }
        fs::write(custom.join("notes.txt"), b"not audio").unwrap();
        let resolver = Resolver::new(vec![custom.clone(), base.clone()]);
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
        assert_eq!(catalog.len(), 3);
        assert!(catalog[0].effective && catalog[0].available);
        assert_eq!(catalog[0].size, wav.len() as u64);
        let shadowed = catalog.iter().find(|entry| !entry.effective).unwrap();
        assert_eq!(shadowed.source, 1);
        assert_eq!(
            resolver.wave_for_file(&path).as_deref(),
            Some("Ambient/Wind.wav")
        );
        assert_eq!(resolver.wave_for_file(&base.join("ambient/wind.wav")), None);
        assert!(
            resolver
                .describe("#sound\\ambient\\wind.WAV")
                .unwrap()
                .contains("mod")
        );
        assert!(resolver.describe("ambient/missing.wav").is_err());
        let metadata = resolver.probe_entry(&catalog[0]).unwrap();
        assert_eq!(metadata.rate, Some(22050));
        assert!(metadata.duration.is_some());
        assert!(
            resolver
                .scan_catalog(|| true, |_| panic!("cancelled scan emitted entries"))
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
