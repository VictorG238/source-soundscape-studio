use crate::decode::Clip;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range(pub f32, pub f32);
impl Range {
    pub fn at(self, t: f32) -> f32 {
        self.0 + (self.1 - self.0) * t
    }
    fn bits(self) -> u64 {
        u64::from(self.0.to_bits()) << 32 | u64::from(self.1.to_bits())
    }
    fn unpack(bits: u64) -> Self {
        Self(
            f32::from_bits((bits >> 32) as u32),
            f32::from_bits(bits as u32),
        )
    }
}
pub struct Levels {
    volume: AtomicU64,
    pitch: AtomicU64,
    interval: AtomicU64,
    pub audible: AtomicBool,
}
impl Levels {
    pub fn new(vol: Range, pitch: Range) -> Self {
        Self {
            volume: AtomicU64::new(vol.bits()),
            pitch: AtomicU64::new(pitch.bits()),
            interval: AtomicU64::new(Range(1., 1.).bits()),
            audible: AtomicBool::new(true),
        }
    }
    pub fn set(&self, vol: Range, pitch: Range) {
        self.volume.store(vol.bits(), Relaxed);
        self.pitch.store(pitch.bits(), Relaxed);
    }
    pub fn set_interval(&self, time: Range) {
        self.interval.store(time.bits(), Relaxed);
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Loop,
    Random(Range),
    Once,
}
#[derive(Clone)]
pub struct Layer {
    pub group: usize,
    pub waves: Vec<String>,
    pub mode: Mode,
    pub levels: Arc<Levels>,
}
impl Layer {
    pub fn new(group: usize, waves: Vec<String>, mode: Mode, volume: Range, pitch: Range) -> Self {
        let levels = Arc::new(Levels::new(volume, pitch));
        if let Mode::Random(time) = mode {
            levels.set_interval(time);
        }
        Self {
            group,
            waves,
            mode,
            levels,
        }
    }
}
pub struct Transport {
    pub stopped: AtomicBool,
    pub paused: AtomicBool,
    pub finished: AtomicBool,
    pub frames: AtomicU64,
    pub rate: AtomicU32,
    pub voices: AtomicU32,
    master: AtomicU32,
    seek: AtomicU64,
    repeat: AtomicBool,
    pub event_count: AtomicU64,
    pub events: [AtomicU64; 8],
}
impl Default for Transport {
    fn default() -> Self {
        Self {
            stopped: false.into(),
            paused: false.into(),
            finished: false.into(),
            frames: 0.into(),
            rate: 0.into(),
            voices: 0.into(),
            master: 0.65f32.to_bits().into(),
            seek: u64::MAX.into(),
            repeat: false.into(),
            event_count: 0.into(),
            events: Default::default(),
        }
    }
}
impl Transport {
    pub fn seek(&self, seconds: f64) {
        self.seek
            .store((seconds.max(0.) * 1_000_000.) as u64, Relaxed);
    }

    pub fn repeat(&self, repeat: bool) {
        self.repeat.store(repeat, Relaxed);
    }
    pub fn volume(&self, value: f32) {
        self.master.store(value.clamp(0., 1.).to_bits(), Relaxed);
    }
    pub fn seconds(&self) -> f64 {
        self.frames.load(Relaxed) as f64 / self.rate.load(Relaxed).max(1) as f64
    }
}
pub(crate) struct Track {
    pub layer: Layer,
    pub clips: Vec<usize>,
    schedule: Option<Schedule>,
}

struct Schedule {
    interval: Range,
    next: u64,
    anchor: u64,
    roll: f32,
    first: bool,
}

impl Schedule {
    fn recalculate(&mut self, rate: u32) {
        let seconds = f64::from(self.interval.at(self.roll));
        let frames = if self.first {
            seconds.max(0.) * 0.5 * f64::from(rate)
        } else {
            (seconds.max(0.01) * f64::from(rate)).max(1.)
        };
        self.next = self.anchor.saturating_add(frames as u64);
    }
}
impl Track {
    pub fn new(layer: Layer, clips: Vec<usize>) -> Self {
        Self {
            layer,
            clips,
            schedule: None,
        }
    }
}
struct Voice {
    track: usize,
    clip: usize,
    pos: f64,
    vol_roll: f32,
    pitch_roll: f32,
    gain: f32,
    step: f64,
}
pub(crate) struct Mixer {
    tracks: Vec<Track>,
    clips: Vec<Arc<Clip>>,
    voices: Vec<Voice>,
    transport: Arc<Transport>,
    frame: u64,
    rate: u32,
    rng: u64,
    master: f32,
    audition: bool,
    repeat: bool,
}

impl Mixer {
    pub fn new(
        tracks: Vec<Track>,
        clips: Vec<Arc<Clip>>,
        transport: Arc<Transport>,
        rate: u32,
        seed: u64,
    ) -> Self {
        transport.rate.store(rate, Relaxed);
        let mut m = Self {
            tracks,
            clips,
            voices: Vec::with_capacity(128),
            transport,
            frame: 0,
            rate,
            rng: seed.max(1),
            master: 0.65,
            audition: false,
            repeat: false,
        };
        for i in 0..m.tracks.len() {
            match m.tracks[i].layer.mode {
                Mode::Random(time) => {
                    let mut schedule = Schedule {
                        interval: time,
                        next: 0,
                        anchor: 0,
                        roll: m.roll(),
                        first: true,
                    };
                    schedule.recalculate(rate);
                    m.tracks[i].schedule = Some(schedule);
                }
                _ => m.start(i),
            }
        }
        m
    }
    pub fn audition(mut self) -> Self {
        self.audition = true;
        self
    }
    fn roll(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as f32) / 16777216.
    }
    fn start(&mut self, i: usize) {
        if self.voices.len() == self.voices.capacity() || self.tracks[i].clips.is_empty() {
            return;
        }
        let choice = (self.roll() * self.tracks[i].clips.len() as f32) as usize;
        let clip = self.tracks[i].clips[choice];
        let mut voice = Voice {
            track: i,
            clip,
            pos: 0.,
            vol_roll: self.roll(),
            pitch_roll: self.roll(),
            gain: 0.,
            step: 1.,
        };
        Self::tune(&mut voice, &self.tracks, &self.clips, self.rate);
        self.voices.push(voice);
        if matches!(self.tracks[i].layer.mode, Mode::Random(_)) {
            let n = self.transport.event_count.load(Relaxed);
            self.transport.events[n as usize % 8].store(
                ((self.frame / self.rate as u64) << 32) | clip as u64,
                Relaxed,
            );
            self.transport
                .event_count
                .store(n + 1, std::sync::atomic::Ordering::Release);
        }
    }
    fn tune(v: &mut Voice, tracks: &[Track], clips: &[Arc<Clip>], rate: u32) {
        let levels = &tracks[v.track].layer.levels;
        v.gain = if levels.audible.load(Relaxed) {
            Range::unpack(levels.volume.load(Relaxed)).at(v.vol_roll)
        } else {
            0.
        };
        v.step = f64::from(clips[v.clip].info.rate) / f64::from(rate)
            * f64::from(
                Range::unpack(levels.pitch.load(Relaxed))
                    .at(v.pitch_roll)
                    .clamp(1., 800.),
            )
            / 100.;
    }
    pub fn begin(&mut self) -> bool {
        if self.transport.stopped.load(Relaxed) {
            self.voices.clear();
            self.transport.voices.store(0, Relaxed);
            return false;
        }
        if self.audition {
            self.repeat = self.transport.repeat.load(Relaxed);
            let micros = self.transport.seek.swap(u64::MAX, Relaxed);
            if micros != u64::MAX
                && let Some(voice) = self.voices.first_mut()
            {
                let clip = &self.clips[voice.clip];
                voice.pos = (micros as f64 * f64::from(clip.info.rate) / 1_000_000.)
                    .min((clip.frames.len() - 1) as f64);
                self.end();
            }
        }
        if self.transport.paused.load(Relaxed) || self.transport.finished.load(Relaxed) {
            return false;
        }
        self.master = f32::from_bits(self.transport.master.load(Relaxed));
        for track in &mut self.tracks {
            if let Some(schedule) = &mut track.schedule {
                let interval = Range::unpack(track.layer.levels.interval.load(Relaxed));
                if interval != schedule.interval {
                    schedule.interval = interval;
                    schedule.recalculate(self.rate);
                }
            }
        }
        for v in &mut self.voices {
            Self::tune(v, &self.tracks, &self.clips, self.rate);
        }
        true
    }
    pub fn sample(&mut self) -> [f32; 2] {
        for i in 0..self.tracks.len() {
            if self.tracks[i]
                .schedule
                .as_ref()
                .is_some_and(|schedule| self.frame >= schedule.next)
            {
                let roll = self.roll();
                let schedule = self.tracks[i].schedule.as_mut().unwrap();
                schedule.anchor = self.frame;
                schedule.roll = roll;
                schedule.first = false;
                schedule.recalculate(self.rate);
                if self.tracks[i].layer.levels.audible.load(Relaxed) {
                    self.start(i);
                }
            }
        }
        let mut out = [0.; 2];
        self.voices.retain_mut(|v| {
            let clip = &self.clips[v.clip];
            let frames = &clip.frames;
            let idx = v.pos as usize;
            let looping = self.tracks[v.track].layer.mode == Mode::Loop || self.repeat;
            let points = self
                .repeat
                .then_some(clip.info.loop_points.as_ref())
                .flatten();
            let (start, end) = points.map_or((0, frames.len()), |points| {
                (points.frames.start, points.frames.end)
            });
            let next = if looping && idx + 1 >= end {
                start
            } else if idx + 1 < frames.len() {
                idx + 1
            } else {
                idx
            };
            for (ch, y) in out.iter_mut().enumerate() {
                *y += (frames[idx][ch]
                    + (frames[next][ch] - frames[idx][ch]) * v.pos.fract() as f32)
                    * v.gain;
            }
            v.pos += v.step;
            if v.pos >= end as f64 && looping {
                v.pos = start as f64 + (v.pos - end as f64) % ((end - start) as f64);
            }
            v.pos < frames.len() as f64
        });
        self.frame += 1;
        out.map(|v| (v * self.master).clamp(-1., 1.))
    }
    pub fn end(&self) {
        let frame = if self.audition {
            let clip = &self.clips[0];
            let pos = self
                .voices
                .first()
                .map_or(clip.frames.len() as f64, |voice| voice.pos);
            (pos * f64::from(self.rate) / f64::from(clip.info.rate)) as u64
        } else {
            self.frame
        };
        self.transport.frames.store(frame, Relaxed);
        self.transport
            .voices
            .store(self.voices.len() as u32, Relaxed);
        if self.voices.is_empty() && self.tracks.iter().all(|t| t.layer.mode == Mode::Once) {
            self.transport.finished.store(true, Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::Info;
    fn clip(sample: f32) -> Arc<Clip> {
        Arc::new(Clip {
            frames: vec![[sample, sample]; 8],
            info: Info {
                rate: 100,
                channels: 2,
                codec: "test".into(),
                duration: 0.08,
                wave: vec![],
                loop_points: None,
            },
        })
    }
    #[test]
    fn loop_and_scheduled_variants_mix_on_the_same_clock() {
        let t = Arc::new(Transport::default());
        t.volume(1.);
        let layers = [Mode::Loop, Mode::Random(Range(0.2, 0.2))];
        let tracks = layers
            .into_iter()
            .enumerate()
            .map(|(i, mode)| {
                Track::new(
                    Layer::new(i, vec![], mode, Range(1., 1.), Range(100., 100.)),
                    if i == 0 { vec![0] } else { vec![1, 2] },
                )
            })
            .collect();
        let mut mix = Mixer::new(
            tracks,
            vec![clip(0.2), clip(0.3), clip(0.4)],
            t.clone(),
            100,
            1234,
        );
        assert!(mix.begin());
        for frame in 0..100 {
            let x = mix.sample()[0];
            let overlap = frame >= 10 && (frame - 10) % 20 < 8;
            assert_eq!(x > 0.4, overlap, "frame {frame}");
            if !overlap {
                assert_eq!(x, 0.2);
            }
        }
        mix.end();
        assert_eq!(t.event_count.load(Relaxed), 5);
        t.paused.store(true, Relaxed);
        assert!(!mix.begin());
        assert_eq!(t.seconds(), 1.);
        t.paused.store(false, Relaxed);
        t.stopped.store(true, Relaxed);
        assert!(!mix.begin());
        assert_eq!(t.voices.load(Relaxed), 0);
    }
    #[test]
    fn audition_seeks_while_paused_and_repeats_marked_frames() {
        let transport = Arc::new(Transport::default());
        transport.volume(1.);
        transport.repeat(true);
        let mut audio = clip(0.);
        let data = Arc::get_mut(&mut audio).unwrap();
        data.frames = (0..8).map(|frame| [frame as f32 / 8.; 2]).collect();
        data.info.loop_points = Some(crate::decode::LoopPoints {
            frames: 2..6,
            marker: crate::decode::LoopMarker::Sampler,
        });
        let layer = Layer::new(0, vec![], Mode::Once, Range(1., 1.), Range(100., 100.));
        let mut mixer = Mixer::new(
            vec![Track::new(layer, vec![0])],
            vec![audio],
            transport.clone(),
            100,
            1,
        )
        .audition();
        transport.paused.store(true, Relaxed);
        transport.seek(0.05);
        assert!(!mixer.begin());
        assert_eq!(transport.seconds(), 0.05);
        transport.paused.store(false, Relaxed);
        assert!(mixer.begin());
        assert_eq!(mixer.sample()[0], 5. / 8.);
        assert_eq!(mixer.sample()[0], 2. / 8.);
        mixer.end();
        assert_eq!(transport.seconds(), 0.03);
    }

    #[test]
    fn changing_random_interval_reschedules_on_the_current_transport() {
        let transport = Arc::new(Transport::default());
        let layer = Layer::new(
            0,
            vec![],
            Mode::Random(Range(10., 10.)),
            Range(1., 1.),
            Range(100., 100.),
        );
        let controls = layer.levels.clone();
        let mut mixer = Mixer::new(
            vec![Track::new(layer, vec![0])],
            vec![clip(0.25)],
            transport.clone(),
            100,
            7,
        );
        assert!(mixer.begin());
        for _ in 0..10 {
            assert_eq!(mixer.sample(), [0.; 2]);
        }
        controls.set_interval(Range(0.25, 0.25));
        assert!(mixer.begin());
        // The original half-interval is shortened relative to the existing clock.
        for _ in 0..2 {
            assert_eq!(mixer.sample(), [0.; 2]);
        }
        assert!(mixer.sample()[0] > 0.);
        mixer.end();
        assert_eq!(transport.event_count.load(Relaxed), 1);
        assert_eq!(transport.seconds(), 0.13);
    }

    #[test]
    fn active_voice_obeys_live_gain_pitch_mute_and_master() {
        let t = Arc::new(Transport::default());
        t.volume(0.5);
        let mut data = clip(0.);
        Arc::get_mut(&mut data).unwrap().frames = (0..8).map(|x| [x as f32 / 8.; 2]).collect();
        let layer = Layer::new(0, vec![], Mode::Loop, Range(1., 1.), Range(100., 100.));
        let controls = layer.levels.clone();
        let mut m = Mixer::new(vec![Track::new(layer, vec![0])], vec![data], t, 100, 7);
        assert!(m.begin());
        assert_eq!(m.sample()[0], 0.);
        controls.set(Range(0.5, 0.5), Range(200., 200.));
        m.begin();
        assert_eq!(m.sample()[0], 1. / 32.);
        assert_eq!(m.sample()[0], 3. / 32.);
        controls.audible.store(false, Relaxed);
        m.begin();
        assert_eq!(m.sample(), [0., 0.]);
    }
}
