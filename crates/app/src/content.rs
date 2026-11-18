use audio_engine::assets::{case_path, is_vpk, vpk_index_path};
use soundscape_core::keyvalues::{Entry, Tree, Value, token};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn block<'a>(entries: &'a [Entry], bytes: &[u8], name: &str) -> &'a [Entry] {
    entries
        .iter()
        .find_map(|entry| {
            if token(bytes, entry.key.clone()).eq_ignore_ascii_case(name)
                && let Value::Block(children) = &entry.value
            {
                return Some(children.as_slice());
            }
            None
        })
        .unwrap_or_default()
}

fn add(mounts: &mut Vec<PathBuf>, path: PathBuf) {
    if let Ok(path) = case_path(&path)
        && (path.is_dir() || (is_vpk(&path) && path.is_file()))
        && !mounts.contains(&path)
    {
        mounts.push(path);
    }
}

fn mount(mounts: &mut Vec<PathBuf>, path: &Path) {
    if is_vpk(path) {
        add(mounts, vpk_index_path(path));
    } else {
        // L4D and later branches can mount pak01 from a Game directory
        // without listing the package explicitly in SearchPaths.
        add(mounts, path.join("pak01_dir.vpk"));
        add(mounts, path.join("sound"));
    }
}

fn children(path: &Path) -> Vec<PathBuf> {
    let Some(path) = case_path(path).ok() else {
        return Vec::new();
    };
    let mut paths: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort_by_cached_key(|path| path.to_string_lossy().to_ascii_lowercase());
    paths
}

fn search_paths(mounts: &mut Vec<PathBuf>, game: &Path, bytes: &[u8]) {
    let tree = Tree::parse(bytes);
    let info = block(&tree.entries, bytes, "GameInfo");
    let filesystem = block(info, bytes, "FileSystem");
    let base = game.parent().unwrap_or(game);
    for entry in block(filesystem, bytes, "SearchPaths") {
        let ids = token(bytes, entry.key.clone());
        if !ids
            .split('+')
            .any(|id| id.eq_ignore_ascii_case("game") || id.eq_ignore_ascii_case("mod"))
        {
            continue;
        }
        let Value::Text(value) = &entry.value else {
            continue;
        };
        let value = token(bytes, value.clone()).replace('\\', "/");
        let lower = value.to_ascii_lowercase();
        let path = if lower.starts_with("|gameinfo_path|") {
            game.join(value[15..].trim_start_matches('/'))
        } else if lower.starts_with("|all_source_engine_paths|") {
            base.join(value[25..].trim_start_matches('/'))
        } else if !value.contains('|') {
            base.join(&value)
        } else {
            continue;
        };
        if path.file_name().is_some_and(|name| name == "*") {
            if let Some(parent) = path.parent() {
                for child in children(parent) {
                    let stem = child.file_stem().unwrap_or_default().to_string_lossy();
                    let chunk = stem.rsplit_once('_').is_some_and(|(_, suffix)| {
                        suffix.len() == 3 && suffix.bytes().all(|b| b.is_ascii_digit())
                    });
                    if !is_vpk(&child) || !chunk {
                        mount(mounts, &child);
                    }
                }
            }
        } else {
            mount(mounts, &path);
        }
    }
}

pub(super) fn discover(script: &Path) -> Vec<PathBuf> {
    game_root(script).map(game_sources).unwrap_or_default()
}

pub(super) fn game_root(script: &Path) -> Option<&Path> {
    script
        .ancestors()
        .skip(1)
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("scripts"))
        })
        .and_then(Path::parent)
}

pub(super) fn game_sources(game: &Path) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    if let Ok(path) = case_path(&game.join("gameinfo.txt"))
        && let Ok(file) = fs::File::open(path)
        && file.metadata().is_ok_and(|meta| meta.len() <= 1024 * 1024)
    {
        use std::io::Read;
        let mut bytes = Vec::new();
        if file.take(1024 * 1024).read_to_end(&mut bytes).is_ok() {
            search_paths(&mut mounts, game, &bytes);
        }
    }
    mount(&mut mounts, game);
    for path in children(game) {
        if is_vpk(&path)
            && path.file_stem().is_some_and(|stem| {
                stem.to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with("_dir")
            })
        {
            add(&mut mounts, path);
        }
    }
    mounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_search_paths_in_engine_order_and_keeps_script_local_sound() {
        let root = std::env::temp_dir().join(format!("sss-gameinfo-{}", std::process::id()));
        let game = root.join("MyMod");
        let custom = game.join("custom/Patch/Sound");
        let base_sound = root.join("base/sound");
        let sound = game.join("sound");
        fs::create_dir_all(&custom).unwrap();
        fs::create_dir_all(&base_sound).unwrap();
        fs::create_dir_all(&sound).unwrap();
        let index = root.join("base/content_dir.vpk");
        fs::write(&index, []).unwrap();
        let implicit = root.join("base/pak01_dir.vpk");
        fs::write(&implicit, []).unwrap();
        let info = game.join("gameinfo.txt");
        fs::write(
            &info,
            br#"GameInfo { FileSystem { SearchPaths {
            game+mod "|gameinfo_path|custom/*"
            game "|all_source_engine_paths|base/content.vpk"
            gamebin "base"
            game "base"
        } } }"#,
        )
        .unwrap();
        assert_eq!(
            discover(&game.join("scripts/maps/soundscapes_custom.txt")),
            [&custom, &index, &implicit, &base_sound, &sound].map(PathBuf::from)
        );
        assert_eq!(game_sources(&game), discover(&game.join("scripts/a.txt")));
        assert!(discover(&root.join("export/a.txt")).is_empty());
        fs::remove_file(info).unwrap();
        fs::remove_file(index).unwrap();
        fs::remove_file(implicit).unwrap();
        for dir in [
            custom,
            game.join("custom/Patch"),
            game.join("custom"),
            base_sound,
            root.join("base"),
            sound,
            game,
            root,
        ] {
            fs::remove_dir(dir).unwrap();
        }
    }
}
