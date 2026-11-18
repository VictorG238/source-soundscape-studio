use soundscape_core::Script;
use std::fs;

const FIXTURE: &str = include_str!("../../../tests/fixtures/harbor.txt");

#[test]
fn visual_commands_round_trip_across_two_soundscapes() {
    let dir = std::env::temp_dir().join(format!("visual-roundtrip-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let file = dir.join("harbor.txt");
    let original = format!("\u{feff}{}", FIXTURE.replace('\n', "\r\n"));
    fs::write(&file, &original).unwrap();
    let mut doc = Script::open(&file).unwrap();
    let scene = doc.entry_at(&[1]).unwrap().key.clone();
    let mut snapshots = vec![doc.bytes().to_vec()];
    macro_rules! change {
        ($expr:expr) => {{
            $expr.unwrap();
            snapshots.push(doc.bytes().to_vec());
        }};
    }
    change!(doc.add_layer(&scene, "playlooping"));
    change!(doc.set_property(&[1, 1], "wave", "harbor/fog.wav"));
    change!(doc.set_property(&[1, 1], "pitch", "85,105"));
    change!(doc.set_property(&[1, 1], "position", "3"));
    change!(doc.add_layer(&scene, "playrandom"));
    change!(doc.add_variant(&[1, 2], "harbor/chain.wav"));
    change!(doc.add_variant(&[1, 2], "harbor/bell.wav"));
    let empty = doc.entry_at(&[1, 2, 1, 0]).unwrap().span();
    change!(doc.remove_entry(empty));
    change!(doc.set_property(&[1, 2], "time", "2.5,6"));
    let r = doc.entry_at(&[1, 2]).unwrap().span();
    change!(doc.duplicate_entry(r));
    let a = doc.entry_at(&[1, 1]).unwrap().span();
    let b = doc.entry_at(&[1, 3]).unwrap().span();
    change!(doc.swap_entries(a, b));
    change!(doc.add_layer(&scene, "playsoundscape"));
    change!(doc.set_property(&[1, 4], "name", "harbor.exterior"));
    change!(doc.copy_soundscape(1));
    let key = doc.entry_at(&[2]).unwrap().key.clone();
    change!(doc.edit_token(key, "harbor.deck"));
    change!(doc.add_soundscape());
    let last = doc.entry_at(&[3]).unwrap().span();
    change!(doc.remove_entry(last));
    for previous in snapshots.iter().rev().skip(1) {
        doc.history(false).unwrap();
        assert_eq!(doc.bytes(), previous);
    }
    assert!(!doc.modified());
    for next in snapshots.iter().skip(1) {
        doc.history(true).unwrap();
        assert_eq!(doc.bytes(), next);
    }
    doc.save(&file).unwrap();
    let loaded = Script::open(&file).unwrap();
    assert_eq!(loaded.bytes(), doc.bytes());
    assert_eq!(loaded.tree.entries.len(), 3);
    assert_eq!(loaded.entry_at(&[2]).unwrap().name(&loaded), "harbor.deck");
    assert_eq!(
        loaded.entry_at(&[1, 1]).unwrap().name(&loaded),
        "playrandom"
    );
    let group = loaded.entry_at(&[1, 1, 1]).unwrap();
    assert_eq!(
        group
            .named(&loaded, "wave")
            .map(|v| v.text(&loaded).unwrap())
            .collect::<Vec<_>>(),
        ["harbor/chain.wav", "harbor/bell.wav"]
    );
    let untouched = original.split("\"harbor.interior\"\r\n{").next().unwrap();
    assert!(loaded.bytes().starts_with(untouched.as_bytes()));
    fs::remove_file(file).unwrap();
    fs::remove_dir(dir).unwrap();
}

#[test]
fn ambiguous_properties_and_unknown_groups_survive_edits() {
    let file = std::env::temp_dir().join(format!("visual-preserve-{}.txt", std::process::id()));
    let raw=b"room { playrandom { volume 1 VoLuMe .5 rndwave { wave a } rndwave { wave b } } playrandom { // \xff\r\n custom { q x q y } } }";
    fs::write(&file, raw).unwrap();
    let mut doc = Script::open(&file).unwrap();
    assert!(doc.set_property(&[0, 0], "volume", ".2").is_err());
    assert!(doc.add_variant(&[0, 0], "x").is_err());
    assert_eq!(doc.bytes(), raw);
    doc.add_variant(&[0, 1], "ambient/b.wav").unwrap();
    assert_eq!(
        doc.entry_at(&[0, 1, 1, 0]).unwrap().text(&doc).unwrap(),
        "ambient/b.wav"
    );
    doc.history(false).unwrap();
    assert_eq!(doc.bytes(), raw);
    fs::remove_file(file).unwrap();
}
