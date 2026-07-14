//! Minimal Standard MIDI File (format 1) writer — just enough of the spec to match
//! what export::midi needs (meta events, note on/off, pitch bend, control change).

use std::io::Write;

pub struct MidiFile {
    pub ticks_per_beat: u16,
    pub tracks: Vec<Track>,
}

#[derive(Default)]
pub struct Track {
    /// (absolute_tick, raw_event_bytes_without_delta_time)
    events: Vec<(u32, Vec<u8>)>,
}

impl Track {
    pub fn push(&mut self, abs_tick: u32, bytes: Vec<u8>) {
        self.events.push((abs_tick, bytes));
    }

    pub fn track_name(&mut self, name: &str) {
        let mut bytes = vec![0xFF, 0x03];
        write_vlq(&mut bytes, name.len() as u32);
        bytes.extend_from_slice(name.as_bytes());
        self.push(0, bytes);
    }

    pub fn set_tempo(&mut self, abs_tick: u32, microseconds_per_beat: u32) {
        let bytes = vec![
            0xFF, 0x51, 0x03,
            ((microseconds_per_beat >> 16) & 0xFF) as u8,
            ((microseconds_per_beat >> 8) & 0xFF) as u8,
            (microseconds_per_beat & 0xFF) as u8,
        ];
        self.push(abs_tick, bytes);
    }

    pub fn note_on(&mut self, abs_tick: u32, channel: u8, note: u8, velocity: u8) {
        self.push(abs_tick, vec![0x90 | (channel & 0x0F), note & 0x7F, velocity & 0x7F]);
    }

    pub fn note_off(&mut self, abs_tick: u32, channel: u8, note: u8, velocity: u8) {
        self.push(abs_tick, vec![0x80 | (channel & 0x0F), note & 0x7F, velocity & 0x7F]);
    }

    pub fn control_change(&mut self, abs_tick: u32, channel: u8, controller: u8, value: u8) {
        self.push(abs_tick, vec![0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F]);
    }

    pub fn pitchwheel(&mut self, abs_tick: u32, channel: u8, pitch: i16) {
        // mido convention: pitch in [-8192, 8191], centered at 0; the wire value is
        // pitch + 8192 as an unsigned 14-bit int, LSB (7 bits) then MSB (7 bits).
        let value = (pitch as i32 + 8192) as u16;
        let lsb = (value & 0x7F) as u8;
        let msb = ((value >> 7) & 0x7F) as u8;
        self.push(abs_tick, vec![0xE0 | (channel & 0x0F), lsb, msb]);
    }

    fn write_to<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        let mut events = self.events.clone();
        events.sort_by_key(|(tick, _)| *tick);

        let mut body = Vec::new();
        let mut last_tick = 0u32;
        // running status: consecutive channel-voice events (status byte < 0xF0) sharing the
        // same status byte can omit repeating it — standard SMF size optimization, matching
        // what general-purpose MIDI writers (e.g. mido) do by default.
        let mut running_status: Option<u8> = None;
        for (abs_tick, bytes) in &events {
            write_vlq(&mut body, abs_tick - last_tick);
            let status = bytes[0];
            if status < 0xF0 && running_status == Some(status) {
                body.extend_from_slice(&bytes[1..]);
            } else {
                body.extend_from_slice(bytes);
                running_status = if status < 0xF0 { Some(status) } else { None };
            }
            last_tick = *abs_tick;
        }
        // end of track meta event
        write_vlq(&mut body, 0);
        body.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        w.write_all(b"MTrk")?;
        w.write_all(&(body.len() as u32).to_be_bytes())?;
        w.write_all(&body)?;
        Ok(())
    }
}

fn write_vlq(out: &mut Vec<u8>, mut value: u32) {
    let mut stack = vec![(value & 0x7F) as u8];
    value >>= 7;
    while value > 0 {
        stack.push(((value & 0x7F) as u8) | 0x80);
        value >>= 7;
    }
    stack.reverse();
    out.extend_from_slice(&stack);
}

impl MidiFile {
    pub fn new(ticks_per_beat: u16) -> Self {
        MidiFile { ticks_per_beat, tracks: Vec::new() }
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut w = std::io::BufWriter::new(file);
        w.write_all(b"MThd")?;
        w.write_all(&6u32.to_be_bytes())?;
        w.write_all(&1u16.to_be_bytes())?; // format 1
        w.write_all(&(self.tracks.len() as u16).to_be_bytes())?;
        w.write_all(&self.ticks_per_beat.to_be_bytes())?;
        for track in &self.tracks {
            track.write_to(&mut w)?;
        }
        Ok(())
    }
}
