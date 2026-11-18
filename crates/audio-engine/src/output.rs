use crate::{
    assets::{Resolver, Sources},
    decode::{Clip, Info},
    mixer::{Layer, Mixer, Mode, Range, Track, Transport},
};
use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::Ordering::Relaxed,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub enum Notice {
    Ready {
        names: Vec<String>,
        info: Option<Info>,
    },
    Warning(String),
    Error(String),
}
pub struct Player {
    pub transport: Arc<Transport>,
    rx: Receiver<Notice>,
    loader: Arc<Mutex<()>>,
}
impl Default for Player {
    fn default() -> Self {
        Self {
            transport: Default::default(),
            rx: mpsc::channel().1,
            loader: Default::default(),
        }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Player {
    pub fn play_clip(&mut self, clip: Arc<Clip>, master: f32, position: f64, repeat: bool) {
        self.stop();
        self.transport = Default::default();
        self.transport.volume(master);
        self.transport.seek(position);
        self.transport.repeat(repeat);
        let transport = self.transport.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = rx;
        thread::spawn(move || {
            let layer = Layer::new(0, vec![], Mode::Once, Range(1., 1.), Range(100., 100.));
            if let Err(error) = play_tracks(
                vec![Track::new(layer, vec![0])],
                vec![clip],
                &transport,
                &tx,
                vec![],
                None,
                true,
            ) && !transport.stopped.load(Relaxed)
            {
                transport.finished.store(true, Relaxed);
                let _ = tx.send(Notice::Error(error.to_string()));
            }
        });
    }
    pub fn stop(&self) {
        self.transport.stopped.store(true, Relaxed);
    }
    pub fn notices(&self) -> impl Iterator<Item = Notice> + '_ {
        self.rx.try_iter()
    }
    pub fn play(&mut self, layers: Vec<Layer>, sources: Sources, master: f32, individual: bool) {
        self.play_resolved(layers, Resolver::new(sources.roots), master, individual);
    }
    pub fn play_resolved(
        &mut self,
        layers: Vec<Layer>,
        sources: Resolver,
        master: f32,
        individual: bool,
    ) {
        self.play_resolved_with_pause(layers, sources, master, individual, false);
    }

    pub fn play_resolved_with_pause(
        &mut self,
        layers: Vec<Layer>,
        sources: Resolver,
        master: f32,
        individual: bool,
        paused: bool,
    ) {
        self.stop();
        self.transport = Default::default();
        self.transport.volume(master);
        self.transport.paused.store(paused, Relaxed);
        let t = self.transport.clone();
        let loader = self.loader.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = rx;
        thread::spawn(move || {
            if let Err(e) = run(layers, sources, t.clone(), loader, &tx, individual)
                && !t.stopped.load(Relaxed)
            {
                t.finished.store(true, Relaxed);
                let _ = tx.send(Notice::Error(e.to_string()));
            }
        });
    }
}
fn run(
    layers: Vec<Layer>,
    sources: Resolver,
    t: Arc<Transport>,
    loader: Arc<Mutex<()>>,
    tx: &Sender<Notice>,
    individual: bool,
) -> io::Result<()> {
    let guard = loader
        .lock()
        .map_err(|_| io::Error::other("Audio loader failed"))?;
    if layers.len() > 128 {
        return Err(io::Error::other("Preview supports up to 128 layers"));
    }
    let mut cache: HashMap<String, Option<usize>> = HashMap::new();
    let mut budget = 256 * 1024 * 1024;
    let mut tracks = Vec::new();
    let mut clips = Vec::new();
    let mut names = Vec::new();
    let mut warnings = Vec::new();
    for layer in layers {
        let mut ids = Vec::new();
        for wave in &layer.waves {
            if t.stopped.load(Relaxed) {
                return Ok(());
            }
            let id = *cache.entry(wave.clone()).or_insert_with(|| {
                let cancel = || t.stopped.load(Relaxed);
                let result = sources.decode_wave(wave, budget, cancel);
                match result {
                    Ok(clip) => {
                        budget -= clip.frames.len() * 8;
                        names.push(wave.clone());
                        clips.push(Arc::new(clip));
                        Some(clips.len() - 1)
                    }
                    Err(e) => {
                        if warnings.len() < 12 {
                            warnings.push(format!("{wave}: {e}"));
                        }
                        None
                    }
                }
            });
            if let Some(i) = id {
                ids.push(i)
            }
        }
        if !ids.is_empty() {
            tracks.push(Track::new(layer, ids));
        }
    }
    drop(guard);
    if t.stopped.load(Relaxed) {
        return Ok(());
    }
    if !warnings.is_empty() {
        let _ = tx.send(Notice::Warning(warnings.join(" · ")));
    }
    if tracks.is_empty() {
        return Err(io::Error::other(
            "No playable sounds. Check the sound folders and layer paths.",
        ));
    }
    let info = individual.then(|| clips[0].info.clone());
    play_tracks(tracks, clips, &t, tx, names, info, false)
}

fn play_tracks(
    tracks: Vec<Track>,
    clips: Vec<Arc<Clip>>,
    t: &Arc<Transport>,
    tx: &Sender<Notice>,
    names: Vec<String>,
    info: Option<Info>,
    audition: bool,
) -> io::Result<()> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| io::Error::other("No default audio output device"))?;
    let config = device.default_output_config().map_err(io::Error::other)?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mixer = Mixer::new(tracks, clips, t.clone(), config.sample_rate(), seed);
    let mixer = if audition { mixer.audition() } else { mixer };
    let errors = tx.clone();
    let failed = t.clone();
    let error = move |e: cpal::Error| {
        failed.finished.store(true, Relaxed);
        let _ = errors.send(Notice::Error(e.to_string()));
    };
    let stream = match config.sample_format() {
        SampleFormat::F32 => stream::<f32>(&device, config.into(), mixer, error),
        SampleFormat::I16 => stream::<i16>(&device, config.into(), mixer, error),
        SampleFormat::I32 => stream::<i32>(&device, config.into(), mixer, error),
        SampleFormat::F64 => stream::<f64>(&device, config.into(), mixer, error),
        SampleFormat::U16 => stream::<u16>(&device, config.into(), mixer, error),
        f => {
            return Err(io::Error::other(format!(
                "Unsupported device sample format: {f}"
            )));
        }
    }
    .map_err(io::Error::other)?;
    if t.stopped.load(Relaxed) {
        return Ok(());
    }
    stream.play().map_err(io::Error::other)?;
    let _ = tx.send(Notice::Ready { names, info });
    while !t.stopped.load(Relaxed) && !t.finished.load(Relaxed) {
        thread::sleep(Duration::from_millis(20));
    }
    // Let the final device buffer drain for short one-shots.
    if !t.stopped.load(Relaxed) {
        thread::sleep(Duration::from_millis(80));
    }
    Ok(())
}
fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut mixer: Mixer,
    error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error> {
    let channels = config.channels as usize;
    device.build_output_stream(
        config,
        move |output: &mut [T], _| {
            if !mixer.begin() {
                output.fill(T::from_sample(0.));
                return;
            }
            for frame in output.chunks_exact_mut(channels) {
                let [l, r] = mixer.sample();
                frame.fill(T::from_sample(0.));
                frame[0] = T::from_sample(if channels == 1 { (l + r) * 0.5 } else { l });
                if channels > 1 {
                    frame[1] = T::from_sample(r);
                }
            }
            mixer.end();
        },
        error,
        None,
    )
}
