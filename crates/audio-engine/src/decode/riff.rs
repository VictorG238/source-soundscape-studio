use super::{LoopMarker, LoopPoints};
use std::io::{Read, Seek, SeekFrom};

#[derive(Default)]
pub(super) struct Markers {
    cue: Option<usize>,
    sampler: Option<std::ops::Range<usize>>,
}

impl Markers {
    pub fn points(self, len: usize) -> Option<LoopPoints> {
        if let Some(start) = self.cue.filter(|&start| start < len) {
            return Some(LoopPoints {
                frames: start..len,
                marker: LoopMarker::Cue,
            });
        }
        self.sampler
            .filter(|range| range.start < range.end && range.end <= len)
            .map(|frames| LoopPoints {
                frames,
                marker: LoopMarker::Sampler,
            })
    }
}

fn number(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

pub(super) fn markers(input: &mut (impl Read + Seek)) -> Markers {
    let mut result = Markers::default();
    let mut header = [0; 12];
    if input.read_exact(&mut header).is_err() || &header[..4] != b"RIFF" || &header[8..] != b"WAVE"
    {
        return result;
    }
    let end = number(&header, 4) as u64 + 8;
    let mut position = 12u64;
    for _ in 0..4096 {
        if position + 8 > end || input.seek(SeekFrom::Start(position)).is_err() {
            break;
        }
        let mut chunk = [0; 8];
        if input.read_exact(&mut chunk).is_err() {
            break;
        }
        let len = number(&chunk, 4);
        let next = position + 8 + len as u64 + (len & 1) as u64;
        if next > end {
            break;
        }
        match &chunk[..4] {
            b"cue " if len >= 28 && result.cue.is_none() => {
                let mut cue = [0; 28];
                if input.read_exact(&mut cue).is_ok()
                    && number(&cue, 0) > 0
                    && &cue[12..16] == b"data"
                {
                    result.cue = Some(number(&cue, 24));
                }
            }
            b"smpl" if len >= 60 && result.sampler.is_none() => {
                let mut loop_data = [0; 60];
                if input.read_exact(&mut loop_data).is_ok()
                    && number(&loop_data, 28) > 0
                    && number(&loop_data, 40) == 0
                    && let Some(end) = number(&loop_data, 48).checked_add(1)
                {
                    result.sampler = Some(number(&loop_data, 44)..end);
                }
            }
            _ => {}
        }
        position = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_forward_loop_markers_and_rejects_out_of_range_offsets() {
        let mut cue = [0; 28];
        cue[0] = 1;
        cue[12..16].copy_from_slice(b"data");
        cue[24] = 10;
        let mut data = b"RIFF\0\0\0\0WAVEcue ".to_vec();
        data.extend(28u32.to_le_bytes());
        data.extend(cue);
        let size = data.len() as u32 - 8;
        data[4..8].copy_from_slice(&size.to_le_bytes());
        let points = markers(&mut std::io::Cursor::new(&data))
            .points(30)
            .unwrap();
        assert_eq!(points.frames, 10..30);
        assert_eq!(points.marker, LoopMarker::Cue);
        assert!(
            markers(&mut std::io::Cursor::new(&data))
                .points(10)
                .is_none()
        );
        assert!(
            markers(&mut std::io::Cursor::new(&data[..25]))
                .points(30)
                .is_none()
        );
    }
}
