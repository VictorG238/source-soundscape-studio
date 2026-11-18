use audio_engine::{assets::Sources, decode::decode};
use std::{fs, path::Path};
const TONES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/audio/sound"
);

#[test]
fn ordered_sources_persistence_and_missing_files() {
    let dir = std::env::temp_dir().join(format!("sss-roots-{}", std::process::id()));
    fs::create_dir_all(dir.join("studio")).unwrap();
    fs::write(dir.join("studio/bed.wav"), []).unwrap();
    let sources = Sources {
        roots: vec![dir.clone(), Path::new(TONES).into()],
    };
    let cfg = dir.join("settings.txt");
    sources.save(&cfg).unwrap();
    let mut loaded = Sources::load(&cfg).unwrap();
    assert_eq!(loaded.roots, sources.roots);
    assert_eq!(
        loaded.resolve("*#studio\\bed.wav").unwrap(),
        dir.join("studio/bed.wav")
    );
    loaded.roots.reverse();
    assert_eq!(
        loaded.resolve("studio/bed.wav").unwrap(),
        Path::new(TONES).join("studio/bed.wav")
    );
    for bad in [
        "missing.wav",
        "../outside.wav",
        "/absolute.wav",
        "C:\\a.wav",
    ] {
        assert!(loaded.resolve(bad).is_none());
    }
    fs::remove_file(dir.join("studio/bed.wav")).unwrap();
    fs::remove_file(cfg).unwrap();
    fs::remove_dir(dir.join("studio")).unwrap();
    fs::remove_dir(dir).unwrap();
}
#[test]
fn pcm_decodes_with_real_samples_and_bounded_waveform() {
    let path = Path::new(TONES).join("studio/bed.wav");
    let clip = decode(&path, 200_000, || false).unwrap();
    assert_eq!(
        (clip.info.rate, clip.info.channels, clip.frames.len()),
        (22050, 1, 11025)
    );
    assert_eq!(clip.info.duration, 0.5);
    assert!(clip.info.wave.len() <= 256 && clip.info.wave.iter().any(|x| *x > 0.3));
    assert!(clip.frames.iter().all(|s| s[0] == s[1]));
    assert!(decode(&path, 1024, || false).is_err());
    assert!(decode(&path, 200_000, || true).is_err());
}

#[test]
fn supported_compressed_formats_produce_nonzero_pcm() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in ["ms.wav", "ima.wav", "chime.mp3", "chime.ogg"] {
        let clip =
            decode(&dir.join(name), 2_000_000, || false).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(clip.info.rate, 22050, "{name}");
        assert!(
            clip.frames.len() > 1000 && clip.info.wave.iter().any(|a| *a > 0.1),
            "{name}"
        );
    }
}
