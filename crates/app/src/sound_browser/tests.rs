use super::*;

fn row(path: &str, source: usize, metadata: Option<Metadata>) -> Row {
    Row {
        asset: CatalogEntry {
            wave: path.into(),
            source,
            size: 1,
            available: true,
            effective: true,
        },
        search: path.to_ascii_lowercase(),
        metadata: metadata.map(Ok),
    }
}

#[test]
fn searches_partial_paths_and_filters_by_format_mount_channels_loop_and_duration() {
    let meta = Metadata {
        rate: Some(44100),
        channels: Some(1),
        duration: Some(1.2),
        loop_marker: Some(LoopMarker::Cue),
    };
    let mut browser = SoundBrowser {
        rows: vec![
            row("Ambient/Wind/North_Wind.WAV", 1, Some(meta)),
            row("Ambient/Wind/North_Wind.mp3", 1, Some(meta)),
            row(
                "Ambient/Wind/North_Whistle.wav",
                2,
                Some(Metadata {
                    channels: Some(2),
                    ..meta
                }),
            ),
            row(
                "Ambient/Wind/North_Short.wav",
                1,
                Some(Metadata {
                    duration: Some(0.1),
                    ..meta
                }),
            ),
            row("Ambient/Wind/North_Unmarked.wav", 1, None),
            row("Ambient/Windstorm/North.wav", 1, Some(meta)),
        ],
        filters: Filters {
            query: "AMBIENT\\WIN nor".into(),
            folder: "ambient/wind".into(),
            format: "wav",
            source: Some(1),
            channels: 1,
            loop_only: true,
            min_seconds: 0.5,
            max_seconds: 2.,
            sort: Sort::Duration,
        },
        ..Default::default()
    };
    browser.filter();
    assert_eq!(browser.visible, [0]);
    browser.filters.source = None;
    browser.filters.channels = 0;
    browser.filters.loop_only = false;
    browser.filters.min_seconds = 0.;
    browser.filters.max_seconds = 0.;
    browser.filter();
    assert_eq!(browser.visible, [3, 2, 0, 4]);
}

#[test]
fn random_selection_applies_once_and_round_trips_losslessly() {
    let root = std::env::temp_dir().join(format!("sss-browser-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let path = root.join("script.txt");
    let original = b"// preserve\r\nscene { playrandom { rndwave { wave old.wav } keep yes } }\r\n";
    fs::write(&path, original).unwrap();
    let mut doc = Script::open(&path).unwrap();
    let mut browser = SoundBrowser {
        rows: vec![
            row("ambient/wind.wav", 0, None),
            row("ambient/bird.wav", 1, None),
        ],
        ..Default::default()
    };
    browser.open(&doc, [0, 0], Pick::Variant);
    browser.select(0, true);
    browser.select(1, true);
    browser.select(0, true);
    let waves = browser.wave_selection();
    browser.close();
    assert_eq!(doc.bytes(), original);
    assert_eq!(waves, ["ambient/wind.wav", "ambient/bird.wav"]);
    browser.open(&doc, [0, 0], Pick::Variant);
    browser
        .target
        .take()
        .unwrap()
        .apply(&mut doc, &waves)
        .unwrap();
    let updated = doc.source_text();
    assert!(updated.contains("// preserve\r\n"));
    assert!(updated.contains("keep yes"));
    let list: Vec<_> = doc
        .entry_at(&[0, 0, 0])
        .unwrap()
        .named(&doc, "wave")
        .map(|entry| entry.text(&doc).unwrap().into_owned())
        .collect();
    assert_eq!(list, ["old.wav", "ambient/wind.wav", "ambient/bird.wav"]);
    doc.history(false).unwrap();
    assert_eq!(doc.bytes(), original);
    assert!(!doc.can_undo());
    doc.history(true).unwrap();
    doc.save(&path).unwrap();
    assert_eq!(Script::open(&path).unwrap().source_text(), updated);
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn looping_selection_keeps_sound_modifiers() {
    let root = std::env::temp_dir().join(format!("sss-browser-loop-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let path = root.join("script.txt");
    fs::write(&path, b"scene { playlooping { wave \"*#old.wav\" } }").unwrap();
    let mut doc = Script::open(&path).unwrap();
    let mut browser = SoundBrowser {
        rows: vec![
            row("ambient/first.wav", 0, None),
            row("ambient/replace.wav", 1, None),
        ],
        ..Default::default()
    };
    browser.open(&doc, [0, 0], Pick::Loop);
    browser.select(0, true);
    browser.select(1, true);
    let selection = browser.wave_selection();
    assert_eq!(selection, ["ambient/replace.wav"]);
    browser
        .target
        .take()
        .unwrap()
        .apply(&mut doc, &selection)
        .unwrap();
    assert!(doc.source_text().contains("\"*#ambient/replace.wav\""));
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn browser_virtualizes_results_and_confirms_from_the_modal() {
    use egui::epaint::Shape;

    fn label_center(shape: &Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            Shape::Vec(shapes) => shapes.iter().find_map(|shape| label_center(shape, label)),
            _ => None,
        }
    }

    let mut browser = SoundBrowser {
        rows: (0..10_000)
            .map(|i| row(&format!("ambient/wind-{i:05}.wav"), 0, None))
            .collect(),
        sources: vec![CatalogSource {
            path: PathBuf::from("mod/sound"),
            packed: false,
        }],
        target: Some(Target {
            path: PathBuf::new(),
            revision: 0,
            layer: [0, 0],
            pick: Pick::Variant,
        }),
        ..Default::default()
    };
    browser.filters.query = "ambient".into();
    browser.select(0, true);
    browser.select(9999, true);
    let ctx = egui::Context::default();
    let render = |browser: &mut SoundBrowser, events, changed| {
        let mut action = BrowserAction::None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1120., 700.),
                )),
                events,
                ..Default::default()
            },
            |_| action = browser.draw(&ctx, changed),
        );
        output.textures_delta.clear();
        (action, output.shapes)
    };
    render(&mut browser, vec![], true);
    for _ in 0..6 {
        render(&mut browser, vec![], false);
    }
    let (_, shapes) = render(&mut browser, vec![], false);
    let position = |text| {
        shapes
            .iter()
            .find_map(|shape| label_center(&shape.shape, text))
    };
    assert!(position("wind-00000.wav").is_some());
    assert!(position("wind-09999.wav").is_none());
    assert!(position("2 selected to add").is_some());
    let button = position("Add Selected").unwrap();
    browser.focused = Some(1);
    browser.preview_status = "Loading audio…".into();
    let (_, loading) = render(&mut browser, vec![], false);
    let add_position = |shapes: &[egui::epaint::ClippedShape]| {
        shapes
            .iter()
            .find_map(|shape| label_center(&shape.shape, "Add Selected"))
    };
    assert_eq!(add_position(&loading), Some(button));
    browser.preview_status.clear();
    browser.zoom = 1.;
    browser.decoded = Some(Decoded {
        clip: Arc::new(Clip {
            frames: vec![[0.25, 0.5]; 8],
            info: audio_engine::decode::Info {
                rate: 8,
                channels: 2,
                codec: "test".into(),
                duration: 1.,
                wave: vec![],
                loop_points: Some(audio_engine::decode::LoopPoints {
                    frames: 1..8,
                    marker: LoopMarker::Cue,
                }),
            },
        }),
        waveform: vec![[[0., 0.25], [0., 0.5]]; 8],
    });
    let (_, ready) = render(&mut browser, vec![], false);
    assert_eq!(add_position(&ready), Some(button));
    assert!(ready.iter().any(|shape| {
        label_center(
            &shape.shape,
            "This WAV has a loop cue. Prefer one-shot sounds in Random layers.",
        )
        .is_some()
    }));
    let event = |pressed| egui::Event::PointerButton {
        pos: button,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let (hover, hovered) = render(&mut browser, vec![egui::Event::PointerMoved(button)], false);
    let hover_position = hovered
        .iter()
        .find_map(|shape| label_center(&shape.shape, "Add Selected"));
    assert!(matches!(hover, BrowserAction::None), "{hover:?}");
    assert_eq!(hover_position, Some(button));
    let (press, _) = render(&mut browser, vec![event(true)], false);
    assert!(matches!(press, BrowserAction::None), "{press:?}");
    let (action, _) = render(&mut browser, vec![event(false)], false);
    assert!(
        matches!(&action, BrowserAction::Apply(waves) if waves == &["ambient/wind-00000.wav", "ambient/wind-09999.wav"]),
        "Button at {button:?}, got {action:?}"
    );
}
