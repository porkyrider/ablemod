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
