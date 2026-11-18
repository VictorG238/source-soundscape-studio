use std::{fs::File, io, path::Path};
use symphonia::core::{
    errors::Error,
    formats::{FormatReader, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
};

mod riff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMarker {
    Cue,
    Sampler,
}

#[derive(Clone, Debug)]
pub struct LoopPoints {
    pub frames: std::ops::Range<usize>,
    pub marker: LoopMarker,
}

fn decode_error(error: Error) -> io::Error {
    match error {
        Error::IoError(error) if error.kind() == io::ErrorKind::PermissionDenied => error,
        Error::Unsupported(_) => io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unsupported audio format: {error}"),
        ),
        _ => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Audio decoding failed: {error}"),
        ),
    }
}

pub struct Clip {
    pub frames: Vec<[f32; 2]>,
    pub info: Info,
}
impl Clip {
    pub fn waveform(&self) -> Vec<[[f32; 2]; 2]> {
        self.frames
            .chunks(self.frames.len().div_ceil(2048).max(1))
            .map(|frames| {
                frames.iter().fold([[0.0f32; 2]; 2], |mut bounds, frame| {
                    for (channel, &sample) in bounds.iter_mut().zip(frame) {
                        channel[0] = channel[0].min(sample);
                        channel[1] = channel[1].max(sample);
                    }
                    bounds
                })
            })
            .collect()
    }
}
#[derive(Clone, Debug)]
pub struct Info {
    pub rate: u32,
    pub channels: usize,
    pub codec: String,
    pub duration: f64,
    pub wave: Vec<f32>,
    pub loop_points: Option<LoopPoints>,
}

#[derive(Clone, Copy, Debug)]
pub struct Metadata {
    pub rate: Option<u32>,
    pub channels: Option<usize>,
    pub duration: Option<f64>,
    pub loop_marker: Option<LoopMarker>,
}

impl Info {
    pub fn metadata(&self) -> Metadata {
        Metadata {
            rate: Some(self.rate),
            channels: Some(self.channels),
            duration: Some(self.duration),
            loop_marker: self.loop_points.as_ref().map(|points| points.marker),
        }
    }
}

pub fn decode(path: &Path, budget: usize, cancel: impl Fn() -> bool) -> io::Result<Clip> {
    decode_source(Box::new(File::open(path)?), path, budget, cancel)
}

fn open_audio(
    mut source: Box<dyn MediaSource>,
    path: &Path,
) -> io::Result<(Box<dyn FormatReader>, riff::Markers)> {
    let loop_markers = if source.is_seekable()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        let markers = riff::markers(&mut source);
        source.seek(io::SeekFrom::Start(0))?;
        markers
    } else {
        Default::default()
    };
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(source, Default::default()),
            Default::default(),
            Default::default(),
        )
        .map_err(decode_error)?;
    Ok((format, loop_markers))
}

pub(crate) fn probe(source: Box<dyn MediaSource>, path: &Path) -> io::Result<Metadata> {
    let (format, markers) = open_audio(source, path)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| io::Error::other("No audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| io::Error::other("No audio codec parameters"))?;
    let channels = params.channels.as_ref().map(|channels| channels.count());
    if channels.is_some_and(|count| !(1..=2).contains(&count)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Preview supports mono and stereo tracks",
        ));
    }
    symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(decode_error)?;
    let rate = params.sample_rate.filter(|rate| *rate > 0);
    let duration = track
        .num_frames
        .zip(rate)
        .map(|(frames, rate)| frames as f64 / f64::from(rate));
    let loop_marker = markers
        .points(
            track
                .num_frames
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(usize::MAX),
        )
        .map(|points| points.marker);
    Ok(Metadata {
        rate,
        channels,
        duration,
        loop_marker,
    })
}

pub(crate) fn decode_source(
    source: Box<dyn MediaSource>,
    path: &Path,
    budget: usize,
    cancel: impl Fn() -> bool,
) -> io::Result<Clip> {
    let (mut format, loop_markers) = open_audio(source, path)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| io::Error::other("No audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| io::Error::other("No audio codec parameters"))?;
    let id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(decode_error)?;
    let codec = decoder.codec_info().short_name.to_string();
    let mut frames = Vec::new();
    let mut samples = Vec::<f32>::new();
    let mut rate = 0;
    let mut channels = 0;
    while let Some(packet) = format.next_packet().map_err(decode_error)? {
        if cancel() {
            return Err(io::Error::other("Preview cancelled"));
        }
        if packet.track_id != id {
            continue;
        }
        let buf = decoder.decode(&packet).map_err(decode_error)?;
        let spec = buf.spec();
        if rate != 0 && (rate != spec.rate() || channels != spec.channels().count()) {
            return Err(io::Error::other("Audio format changes within the track"));
        }
        rate = spec.rate();
        channels = spec.channels().count();
        if !(1..=2).contains(&channels) || rate == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported audio format: preview supports mono and stereo tracks",
            ));
        }
        let len = buf.samples_interleaved();
        if frames.len() + len / channels > budget / 8 {
            return Err(io::Error::other(
                "Decoded audio exceeds preview memory limit",
            ));
        }
        samples.resize(len, 0.);
        buf.copy_to_slice_interleaved(&mut samples);
        frames.extend(
            samples
                .chunks_exact(channels)
                .map(|s| [s[0], s[channels - 1]]),
        );
    }
    if frames.is_empty() {
        return Err(io::Error::other("Empty audio track"));
    }
    let wave = frames
        .chunks(frames.len().div_ceil(256))
        .map(|chunk| {
            chunk
                .iter()
                .fold(0f32, |m, s| m.max(s[0].abs()).max(s[1].abs()))
        })
        .collect();
    let duration = frames.len() as f64 / f64::from(rate);
    let loop_points = loop_markers.points(frames.len());
    Ok(Clip {
        frames,
        info: Info {
            rate,
            channels,
            codec,
            duration,
            wave,
            loop_points,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_decoder_failures_from_unsupported_formats_and_permissions() {
        assert_eq!(
            decode_error(Error::Unsupported("codec")).kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            decode_error(Error::DecodeError("packet")).kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            decode_error(Error::IoError(io::ErrorKind::PermissionDenied.into())).kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
