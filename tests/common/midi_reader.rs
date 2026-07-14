//! Minimal Standard MIDI File reader — just enough to verify export::midi's output in tests
//! (delta-time decoding, meta events we emit, and the channel-voice messages we emit).

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
    ControlChange { control: u8, value: u8 },
    PitchBend { value: i16 },
    SetTempo { tempo: u32 },
    TrackName(String),
}

pub struct Track {
    pub events: Vec<(u32, Msg)>, // (absolute_tick, message)
}

pub struct MidiFile {
    pub ticks_per_beat: u16,
    pub tracks: Vec<Track>,
}

fn read_vlq(data: &[u8], pos: &mut usize) -> u32 {
    let mut value: u32 = 0;
    loop {
        let byte = data[*pos];
        *pos += 1;
        value = (value << 7) | (byte & 0x7F) as u32;
        if byte & 0x80 == 0 {
            break;
        }
    }
    value
}

pub fn parse(path: &std::path::Path) -> MidiFile {
    let data = std::fs::read(path).unwrap();
    assert_eq!(&data[0..4], b"MThd");
    let header_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    assert_eq!(header_len, 6);
    let num_tracks = u16::from_be_bytes([data[10], data[11]]);
    let ticks_per_beat = u16::from_be_bytes([data[12], data[13]]);

    let mut pos = 14usize;
    let mut tracks = Vec::new();
    for _ in 0..num_tracks {
        assert_eq!(&data[pos..pos + 4], b"MTrk");
        pos += 4;
        let track_len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        let end = pos + track_len;

        let mut events = Vec::new();
        let mut abs_tick = 0u32;
        let mut running_status: Option<u8> = None;
        while pos < end {
            let delta = read_vlq(&data, &mut pos);
            abs_tick += delta;

            let mut status = data[pos];
            if status < 0x80 {
                // running status: reuse the previous status byte, this byte is already data
                status = running_status.expect("running status with none set");
            } else {
                pos += 1;
                if status < 0xF0 {
                    running_status = Some(status);
                }
            }

            if status == 0xFF {
                let meta_type = data[pos];
                pos += 1;
                let len = read_vlq(&data, &mut pos) as usize;
                let payload = &data[pos..pos + len];
                pos += len;
                match meta_type {
                    0x03 => events.push((abs_tick, Msg::TrackName(String::from_utf8_lossy(payload).into_owned()))),
                    0x51 => {
                        let tempo = ((payload[0] as u32) << 16) | ((payload[1] as u32) << 8) | payload[2] as u32;
                        events.push((abs_tick, Msg::SetTempo { tempo }));
                    }
                    0x2F => {} // end of track
                    _ => {}
                }
            } else {
                let hi = status & 0xF0;
                match hi {
                    0x90 => {
                        let note = data[pos];
                        let vel = data[pos + 1];
                        pos += 2;
                        if vel == 0 {
                            events.push((abs_tick, Msg::NoteOff { note }));
                        } else {
                            events.push((abs_tick, Msg::NoteOn { note, velocity: vel }));
                        }
                    }
                    0x80 => {
                        let note = data[pos];
                        pos += 2;
                        events.push((abs_tick, Msg::NoteOff { note }));
                    }
                    0xB0 => {
                        let control = data[pos];
                        let value = data[pos + 1];
                        pos += 2;
                        events.push((abs_tick, Msg::ControlChange { control, value }));
                    }
                    0xE0 => {
                        let lsb = data[pos] as i32;
                        let msb = data[pos + 1] as i32;
                        pos += 2;
                        let value = ((msb << 7) | lsb) - 8192;
                        events.push((abs_tick, Msg::PitchBend { value: value as i16 }));
                    }
                    _ => panic!("unsupported status byte {status:#x}"),
                }
            }
        }
        tracks.push(Track { events });
    }

    MidiFile { ticks_per_beat, tracks }
}

pub fn bpm2tempo(bpm: f64) -> u32 {
    (60_000_000.0 / bpm).round() as u32
}

pub fn tempo2bpm(tempo: u32) -> f64 {
    60_000_000.0 / tempo as f64
}
