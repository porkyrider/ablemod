//! Builds a minimal, valid, synthetic .mod file for unit tests.

#[allow(dead_code)]
pub mod midi_reader;

fn sample_header(name: &[u8], length_words: u16, finetune: u8, volume: u8, loop_off_words: u16, loop_len_words: u16) -> Vec<u8> {
    let mut out = vec![0u8; 22];
    let n = name.len().min(22);
    out[..n].copy_from_slice(&name[..n]);
    out.extend_from_slice(&length_words.to_be_bytes());
    out.push(finetune & 0x0F);
    out.push(volume);
    out.extend_from_slice(&loop_off_words.to_be_bytes());
    out.extend_from_slice(&loop_len_words.to_be_bytes());
    out
}

/// One 8-frame sample ('kick'), one pattern, 4 channels, a single note.
///
/// `second_cell_effect`, if given, is an (effect, effect_param) pair written to row 0 /
/// channel 1 (no sample/note attached) — for tests that need a module carrying a specific
/// effect without disturbing the base note in channel 0.
#[allow(dead_code)]
pub fn build_minimal_mod(second_cell_effect: Option<(u8, u8)>) -> Vec<u8> {
    let mut title = b"TESTMOD".to_vec();
    title.resize(20, 0);

    let kick_data: Vec<u8> = vec![0, 40, 80, 120, 127, (120u8 & 0xFF), (200u32 & 0xFF) as u8, (246u32 & 0xFF) as u8];
    let mut headers = sample_header(b"kick", (kick_data.len() / 2) as u16, 0, 64, 0, 0);
    headers.extend(std::iter::repeat(0u8).take(30 * 30)); // 30 empty sample headers (31 total)

    let song_length: u8 = 1;
    let restart: u8 = 0;
    let mut order = vec![0u8];
    order.extend(std::iter::repeat(0u8).take(127));
    let tag = b"M.K.";

    let mut pattern = vec![0u8; 64 * 4 * 4];
    let (sample_num, period, effect, effect_param): (u8, u16, u8, u8) = (1, 428, 0, 0); // C-2, sample 1
    pattern[0] = (sample_num & 0xF0) | (((period >> 8) & 0x0F) as u8);
    pattern[1] = (period & 0xFF) as u8;
    pattern[2] = ((sample_num & 0x0F) << 4) | effect;
    pattern[3] = effect_param;

    if let Some((effect, effect_param)) = second_cell_effect {
        pattern[6] = effect & 0x0F;
        pattern[7] = effect_param;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&title);
    out.extend_from_slice(&headers);
    out.push(song_length);
    out.push(restart);
    out.extend_from_slice(&order);
    out.extend_from_slice(tag);
    out.extend_from_slice(&pattern);
    out.extend_from_slice(&kick_data);
    out
}

fn padded(s: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let n = s.len().min(len);
    out[..n].copy_from_slice(&s[..n]);
    out
}

fn delta_encode_8bit(plaintext: &[i8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(plaintext.len());
    let mut prev: i8 = 0;
    for &v in plaintext {
        out.push(v.wrapping_sub(prev) as u8);
        prev = v;
    }
    out
}

/// A minimal, valid, synthetic .xm file: 4 channels, one 1-sample instrument with a simple
/// 2-point decay volume envelope (enabled) and a disabled panning envelope, one pattern with
/// 2 rows exercising both the uncompressed cell format (row 0 / channel 0: a new note +
/// instrument + volume-column set-volume) and the compressed format (row 0 / channel 1: an
/// effect-only cell; row 1 / channel 0: a Key Off note; every other cell an empty compressed
/// cell). The one 8-frame sample's plaintext waveform matches build_minimal_mod's own "kick"
/// sample byte-for-byte (before delta-encoding) for easy cross-format comparison.
#[allow(dead_code)]
pub fn build_minimal_xm() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"Extended Module: ");
    out.extend_from_slice(&padded(b"TESTXM", 20));
    out.push(0x1A);
    out.extend_from_slice(&padded(b"ablemod test", 20));
    out.extend_from_slice(&260u16.to_le_bytes()); // version 0x0104

    let header_size: u32 = 276; // 4 (this field) + 16 (fixed fields) + 256 (order table)
    out.extend_from_slice(&header_size.to_le_bytes());

    let num_channels: u16 = 4;
    out.extend_from_slice(&1u16.to_le_bytes()); // song length
    out.extend_from_slice(&0u16.to_le_bytes()); // restart position
    out.extend_from_slice(&num_channels.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // num patterns
    out.extend_from_slice(&1u16.to_le_bytes()); // num instruments
    out.extend_from_slice(&1u16.to_le_bytes()); // flags: linear frequency table
    out.extend_from_slice(&6u16.to_le_bytes()); // default speed (ticks/row)
    out.extend_from_slice(&125u16.to_le_bytes()); // default BPM
    let mut order_table = vec![0u8; 256];
    order_table[0] = 0;
    out.extend_from_slice(&order_table);
    assert_eq!(out.len(), 60 + header_size as usize);

    // --- Pattern (2 rows x 4 channels) ---
    let mut cells = Vec::new();
    // Row 0, channel 0: uncompressed cell — note 49 ("C-4"), instrument 1, volume-column set
    // volume to 48 (0x40 = 0x10 + 48), no effect.
    cells.extend_from_slice(&[49, 1, 0x40, 0, 0]);
    // Row 0, channel 1: compressed cell — effect only (volume slide down by 4, no note).
    cells.push(0x80 | 0x08 | 0x10); // mask: effect type + effect param present
    cells.push(0x0A); // effect A = volume slide
    cells.push(0x04); // slide down 4
    // Row 0, channels 2-3: empty compressed cells.
    cells.push(0x80);
    cells.push(0x80);
    // Row 1, channel 0: compressed cell — Key Off note only.
    cells.push(0x80 | 0x01); // mask: note present
    cells.push(97);
    // Row 1, channels 1-3: empty compressed cells.
    cells.push(0x80);
    cells.push(0x80);
    cells.push(0x80);

    out.extend_from_slice(&9u32.to_le_bytes()); // pattern header length
    out.push(0); // packing type
    out.extend_from_slice(&2u16.to_le_bytes()); // num rows
    out.extend_from_slice(&(cells.len() as u16).to_le_bytes()); // packed data size
    out.extend_from_slice(&cells);

    // --- Instrument (1, with 1 sample) ---
    let inst_size: u32 = 263;
    out.extend_from_slice(&inst_size.to_le_bytes());
    out.extend_from_slice(&padded(b"TESTINST", 22));
    out.push(0); // type
    out.extend_from_slice(&1u16.to_le_bytes()); // num samples

    let mut ext = Vec::with_capacity(234);
    ext.extend_from_slice(&40u32.to_le_bytes()); // sample header size
    ext.extend_from_slice(&[0u8; 96]); // keymap: every note -> sample 0 (the only sample)
    // Volume envelope: 12 points, only the first 2 populated (decay 64 -> 0).
    let mut vol_env = vec![0u8; 48];
    vol_env[0..2].copy_from_slice(&0u16.to_le_bytes()); // point 0 tick
    vol_env[2..4].copy_from_slice(&64u16.to_le_bytes()); // point 0 value
    vol_env[4..6].copy_from_slice(&10u16.to_le_bytes()); // point 1 tick
    vol_env[6..8].copy_from_slice(&0u16.to_le_bytes()); // point 1 value
    ext.extend_from_slice(&vol_env);
    ext.extend_from_slice(&[0u8; 48]); // panning envelope points (unused, disabled)
    ext.push(2); // num volume points
    ext.push(0); // num panning points
    ext.push(0); // volume sustain point
    ext.push(0); // volume loop start point
    ext.push(0); // volume loop end point
    ext.push(0); // panning sustain point
    ext.push(0); // panning loop start point
    ext.push(0); // panning loop end point
    ext.push(0x01); // volume type: on, no sustain/loop
    ext.push(0x00); // panning type: off
    ext.extend_from_slice(&[0u8; 4]); // vibrato type/sweep/depth/rate
    ext.extend_from_slice(&0u16.to_le_bytes()); // fadeout
    ext.extend_from_slice(&[0u8; 22]); // reserved
    assert_eq!(ext.len(), 234);
    out.extend_from_slice(&ext);

    // Sample header (40 bytes).
    let plaintext: [i8; 8] = [0, 40, 80, 120, 127, 120, 80, 40];
    let pcm = delta_encode_8bit(&plaintext);
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes()); // length
    out.extend_from_slice(&0u32.to_le_bytes()); // loop start
    out.extend_from_slice(&0u32.to_le_bytes()); // loop length
    out.push(64); // volume
    out.push(0); // finetune
    out.push(0x00); // type: 8-bit, no loop
    out.push(128); // panning: center
    out.push(0); // relative note
    out.push(0); // reserved
    out.extend_from_slice(&padded(b"kick", 22));

    out.extend_from_slice(&pcm);

    out
}
