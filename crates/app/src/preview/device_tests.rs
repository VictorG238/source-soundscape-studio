use super::*;
use std::{path::Path, thread, time::Instant};

#[test]
fn preview_drafts_override_plan_without_mutating_the_script() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/audio");
    let doc = Script::open(&dir.join("soundscapes_studio.txt")).unwrap();
    let source = doc.source_text();
    let drafts = HashMap::from([
        (
            0,
            LayerDraft {
                volume: Some(Range(0.07, 0.07)),
                pitch: Some(Range(80., 80.)),
                ..Default::default()
            },
        ),
        (
            1,
            LayerDraft {
                volume: Some(Range(0.25, 0.25)),
                ..Default::default()
            },
        ),
    ]);
    let plan = Plan::build(std::slice::from_ref(&doc), 0, 0, 42, &drafts);
    assert_eq!(plan.tracks[0].volume, Range(0.07, 0.07));
    assert_eq!(plan.tracks[0].pitch, Range(80., 80.));
    assert_eq!(plan.tracks[1].volume, Range(0.5 * 0.25, 0.8 * 0.25));
    assert_eq!(doc.source_text(), source);
    assert!(!doc.modified());
}

#[test]
#[ignore = "requires a native audio output device"]
fn native_scene_follows_unsaved_drafts_and_wave_edits() {
    let path = PathBuf::from(format!("dist/sss-live-{}.txt", std::process::id()));
    let original = "scene { playrandom { time 100 rndwave { wave studio/bell.wav } } }";
    std::fs::write(&path, original).unwrap();
    let mut docs = [Script::open(&path).unwrap()];
    let mut p = Preview::default();
    p.sources.roots = vec![PathBuf::from("tests/fixtures/audio/sound")];
    p.refresh_sources();
    p.sync(&docs, 0, 0);
    p.start_scene(&docs, 0, 0);
    let old = p.player.transport.clone();
    assert!(p.set_draft(
        docs[0].revision(),
        0,
        LayerDraft {
            interval: Some(Range(0.05, 0.05)),
            ..Default::default()
        }
    ));
    p.sync(&docs, 0, 0);
    assert!(std::sync::Arc::ptr_eq(&old, &p.player.transport));
    let wait_until = Instant::now() + Duration::from_secs(5);
    while old.event_count.load(Acquire) == 0 && Instant::now() < wait_until {
        for notice in p.player.notices() {
            if let Notice::Error(error) | Notice::Warning(error) = notice {
                panic!("{error}");
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        old.event_count.load(Acquire) > 0,
        "Draft interval did not reach the currently playing mix"
    );
    assert!(!docs[0].modified());
    old.paused.store(true, Relaxed);
    docs[0]
        .apply_source(&original.replace("studio/bell.wav", "studio/chime.wav"))
        .unwrap();
    p.sync(&docs, 0, 0);
    assert!(!std::sync::Arc::ptr_eq(&old, &p.player.transport));
    assert!(p.player.transport.paused.load(Relaxed));
    let wait_until = Instant::now() + Duration::from_secs(5);
    let mut replaced = false;
    while !replaced && Instant::now() < wait_until {
        for notice in p.player.notices() {
            match notice {
                Notice::Ready { names, .. } => {
                    assert_eq!(names, ["studio/chime.wav"]);
                    replaced = true;
                }
                Notice::Error(error) | Notice::Warning(error) => panic!("{error}"),
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(replaced);
    p.stop();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    std::fs::remove_file(path).unwrap();
    println!(
        "Updated the playing interval on the same transport and loaded a replacement WAV while paused, without writing the script"
    );
}

#[test]
#[ignore = "requires a real Source mod and native audio output"]
fn native_sound_browser_mod_workflow() {
    let script = PathBuf::from(
        std::env::var_os("SSS_BROWSER_SCRIPT")
            .expect("Set SSS_BROWSER_SCRIPT to a Source soundscape"),
    );
    let original = Script::open(&script).unwrap();
    let mut preview = Preview::default();
    preview.sources.roots.clear();
    preview.reload_content(&original.path);
    let mut count = 0;
    let mut candidates = Vec::new();
    let warnings = preview.resolver.scan_catalog(
        || false,
        |entry| {
            count += 1;
            if entry.effective
                && entry.wave.starts_with("ambient/")
                && entry.wave.ends_with(".wav")
                && entry.wave.contains("wind")
                && candidates.len() < 64
            {
                candidates.push(entry);
            }
            true
        },
    );
    assert!(count > 1000, "catalog only has {count} files: {warnings:?}");
    let mut tracks = Vec::new();
    let mut player = Player::default();
    for entry in candidates {
        let Ok(clip) = preview
            .resolver
            .decode_entry(&entry, 64 * 1024 * 1024, || false)
        else {
            continue;
        };
        let waveform = clip.waveform();
        if !waveform
            .iter()
            .flatten()
            .flatten()
            .any(|&sample| sample != 0.)
        {
            continue;
        }
        let start_at = clip.info.duration / 4.;
        player.play_clip(std::sync::Arc::new(clip), 0.15, start_at, true);
        let start = Instant::now();
        while player.transport.seconds() <= start_at && start.elapsed() < Duration::from_secs(6) {
            for notice in player.notices() {
                if let Notice::Error(error) = notice {
                    panic!("{error}");
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(player.transport.seconds() > start_at);
        player.stop();
        println!(
            "Auditioned {} at {:.3} s with {} waveform bins",
            entry.wave,
            start_at,
            waveform.len()
        );
        tracks.push(entry.wave);
        if tracks.len() == 2 {
            break;
        }
    }
    assert_eq!(tracks.len(), 2);
    let path = PathBuf::from(format!("dist/browser-mod-test-{}.txt", std::process::id()));
    std::fs::write(
        &path,
        b"// Preserve this comment\r\nscene { playrandom { time 0.04 unknown keep } }\r\n",
    )
    .unwrap();
    let mut doc = Script::open(&path).unwrap();
    doc.add_variants(&[0, 0], &tracks).unwrap();
    doc.save(&path).unwrap();
    let reopened = Script::open(&path).unwrap();
    assert!(
        reopened
            .source_text()
            .contains("// Preserve this comment\r\n")
    );
    assert!(reopened.source_text().contains("unknown keep"));
    let plan = Plan::build(&[reopened], 0, 0, 42, &HashMap::new());
    assert_eq!(plan.tracks[0].layer.waves, tracks);
    preview.launch(
        plan.tracks.into_iter().map(|track| track.layer).collect(),
        None,
    );
    let start = Instant::now();
    while preview.player.transport.event_count.load(Acquire) < 2
        && start.elapsed() < Duration::from_secs(6)
    {
        for notice in preview.player.notices() {
            if let Notice::Error(error) | Notice::Warning(error) = notice {
                panic!("{error}");
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(preview.player.transport.event_count.load(Acquire) >= 2);
    preview.stop();
    std::fs::remove_file(path).unwrap();
    println!("Indexed {count} assets. Saved, reopened and played the two rndwave variants.");
}

#[test]
fn preview_keeps_its_dragged_height_after_release() {
    let ctx = egui::Context::default();
    let mut studio = Studio::default();
    let mut frame = |events| {
        let mut top = 0.;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 700.),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                studio.preview_panel(ui);
                top = ui.available_rect_before_wrap().bottom();
            },
        )
        .textures_delta
        .clear();
        top
    };
    frame(vec![]);
    let start = egui::pos2(760., frame(vec![]));
    let end = start - egui::vec2(0., 120.);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(vec![egui::Event::PointerMoved(start)]);
    frame(vec![button(start, true)]);
    frame(vec![egui::Event::PointerMoved(end)]);
    frame(vec![button(end, false)]);
    let top = frame(vec![]);
    assert!(
        (top - end.y).abs() <= 2.,
        "Expected panel edge near {}, got {top}",
        end.y
    );
}

#[test]
fn demo_expands_to_continuous_and_random_tracks() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/audio");
    let script = Script::open(&base.join("soundscapes_studio.txt")).unwrap();
    let p = Plan::build(&[script], 0, 0, 42, &HashMap::new());
    assert!(p.warnings.is_empty());
    assert_eq!(p.tracks.len(), 2);
    assert_eq!(p.tracks[0].layer.mode, Mode::Loop);
    assert_eq!(p.tracks[1].layer.mode, Mode::Random(Range(0.5, 0.9)));
    assert_eq!(p.tracks[1].layer.waves.len(), 2);
    assert_eq!(p.tracks[1].volume, Range(0.5 * 0.85, 0.8 * 0.85));
}

#[test]
#[ignore = "requires an active native output device and the demo fixture in the current workspace"]
fn native_full_preview() {
    let dir = Path::new("tests/fixtures/audio");
    let docs = [Script::open(&dir.join("soundscapes_studio.txt")).unwrap()];
    let mut p = Preview::default();
    p.sources.roots = vec![dir.join("sound")];
    p.refresh_sources();
    p.master = 0.2;
    p.sync(&docs, 0, 0);
    p.start_scene(&docs, 0, 0);
    let start = Instant::now();
    let mut max_voices = 0;
    let mut loaded = false;
    while start.elapsed() < Duration::from_secs(8) {
        for msg in p.player.notices() {
            match msg {
                Notice::Error(m) | Notice::Warning(m) => panic!("{m}"),
                Notice::Ready { names, .. } => {
                    assert_eq!(names.len(), 3);
                    loaded = true;
                }
            }
        }
        let t = &p.player.transport;
        max_voices = max_voices.max(t.voices.load(Relaxed));
        if loaded && max_voices >= 2 && t.event_count.load(Acquire) >= 3 {
            println!(
                "Native output {} Hz, {:.2} s, {max_voices} concurrent voices, {} random events",
                t.rate.load(Relaxed),
                t.seconds(),
                t.event_count.load(Acquire)
            );
            p.stop();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("Native playback failed to produce overlapping voices and three random events");
}

#[test]
#[ignore = "requires a Half-Life 2 installation and native audio output"]
fn native_hl2_vpk_preview() {
    let root = PathBuf::from(
        std::env::var_os("SSS_HL2_CONTENT").expect("Set SSS_HL2_CONTENT to the hl2 directory"),
    );
    let docs = [Script::open(&root.join("scripts/soundscapes_prison.txt")).unwrap()];
    let mounts = crate::content::game_sources(&root);
    assert!(!mounts.is_empty());
    let mut p = Preview {
        detected: crate::content::discover(&docs[0].path),
        ..Default::default()
    };
    p.refresh_sources();
    let wave = "ambient/levels/prison/inside_battle1.wav";
    let location = p.resolver.describe(wave).unwrap();
    assert!(location.contains("Found in VPK"), "{location}");
    let plan = Plan::build(&docs, 0, 0, 42, &HashMap::new());
    let expected = plan
        .tracks
        .iter()
        .map(|track| track.layer.waves.len())
        .sum::<usize>();
    assert!(
        plan.tracks
            .iter()
            .any(|track| track.layer.waves.iter().any(|name| name == wave))
    );
    // A script copied out of the installation still works with a registered game folder.
    p.detected.clear();
    p.sources.roots = mounts;
    p.refresh_sources();
    p.master = 0.15;
    p.audition(wave.into());
    let start = Instant::now();
    let mut decoded = false;
    while start.elapsed() < Duration::from_secs(12) {
        for notice in p.player.notices() {
            match notice {
                Notice::Warning(error) | Notice::Error(error) => panic!("{error}"),
                Notice::Ready { info, names } => {
                    assert_eq!(names, [wave]);
                    let info = info.unwrap();
                    assert!(info.wave.iter().any(|peak| *peak > 0.0));
                    println!(
                        "{location}, {} Hz {}, {:.2} s",
                        info.rate, info.codec, info.duration
                    );
                    decoded = true;
                }
            }
        }
        let transport = &p.player.transport;
        if decoded && transport.seconds() >= 0.1 {
            println!(
                "Native audio output running at {} Hz",
                transport.rate.load(Relaxed)
            );
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        decoded && p.player.transport.seconds() >= 0.1,
        "Native preview did not advance"
    );

    p.start_scene(&docs, 0, 0);
    let start = Instant::now();
    let mut loaded = false;
    while start.elapsed() < Duration::from_secs(20) {
        for notice in p.player.notices() {
            match notice {
                Notice::Warning(error) | Notice::Error(error) => panic!("{error}"),
                Notice::Ready { names, .. } => {
                    assert_eq!(names.len(), expected);
                    loaded = true;
                }
            }
        }
        let transport = &p.player.transport;
        if loaded && transport.event_count.load(Acquire) > 0 && transport.voices.load(Relaxed) > 0 {
            println!("Half-Life 2 scene decoded {expected} sounds and played on the native output");
            p.stop();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("The Half-Life 2 scene did not play a scheduled sound");
}
