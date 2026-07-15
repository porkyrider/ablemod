//! YM2413 (OPLL) emulator — a faithful Rust port of Mitsutaka Okazaki's "emu2413"
//! (<https://github.com/digital-sound-antiques/emu2413>, MIT license), a widely used,
//! well-regarded reference implementation. Kept close to the original C structure/names
//! (slots, patches, envelope generator states) rather than "Rustified" for its own sake, so
//! it stays easy to diff against the reference if a correctness question ever comes up —
//! pointers into shared arrays (OPLL_SLOT.patch, .wave_table) become indices instead, since
//! Rust can't alias those the way C does, but the algorithm itself is untouched.
//!
//! Only the real YM2413 instrument ROM is included (not the VRC7/YMF281B variants
//! emu2413 also supports) — this project only ever targets genuine YM2413 chip data from
//! VGM files.

const DP_BITS: u32 = 19;
const DP_WIDTH: u32 = 1 << DP_BITS;
const PG_BITS: u32 = 10;
const DP_BASE_BITS: u32 = DP_BITS - PG_BITS;
const PG_WIDTH: usize = 1 << PG_BITS;

const EG_STEP: f64 = 0.375;
const EG_BITS: u32 = 7;
const EG_MUTE: u16 = (1 << EG_BITS) - 1;
const EG_MAX: u16 = EG_MUTE - 4;

const DAMPER_RATE: u8 = 12;

const fn tl2eg(d: u32) -> u32 {
    d << 1
}

const SLOT_BD1: usize = 12;
const SLOT_HH: usize = 14;
const SLOT_SD: usize = 15;
const SLOT_TOM: usize = 16;
const SLOT_CYM: usize = 17;

#[rustfmt::skip]
const DEFAULT_INST: [u8; 19 * 8] = [
    0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00, // 0: User
    0x71,0x61,0x1e,0x17,0xd0,0x78,0x00,0x17, // 1: Violin
    0x13,0x41,0x1a,0x0d,0xd8,0xf7,0x23,0x13, // 2: Guitar
    0x13,0x01,0x99,0x00,0xf2,0xc4,0x21,0x23, // 3: Piano
    0x11,0x61,0x0e,0x07,0x8d,0x64,0x70,0x27, // 4: Flute
    0x32,0x21,0x1e,0x06,0xe1,0x76,0x01,0x28, // 5: Clarinet
    0x31,0x22,0x16,0x05,0xe0,0x71,0x00,0x18, // 6: Oboe
    0x21,0x61,0x1d,0x07,0x82,0x81,0x11,0x07, // 7: Trumpet
    0x33,0x21,0x2d,0x13,0xb0,0x70,0x00,0x07, // 8: Organ
    0x61,0x61,0x1b,0x06,0x64,0x65,0x10,0x17, // 9: Horn
    0x41,0x61,0x0b,0x18,0x85,0xf0,0x81,0x07, // A: Synthesizer
    0x33,0x01,0x83,0x11,0xea,0xef,0x10,0x04, // B: Harpsichord
    0x17,0xc1,0x24,0x07,0xf8,0xf8,0x22,0x12, // C: Vibraphone
    0x61,0x50,0x0c,0x05,0xd2,0xf5,0x40,0x42, // D: Synthsizer Bass
    0x01,0x01,0x55,0x03,0xe9,0x90,0x03,0x02, // E: Acoustic Bass
    0x41,0x41,0x89,0x03,0xf1,0xe4,0xc0,0x13, // F: Electric Guitar
    0x01,0x01,0x18,0x0f,0xdf,0xf8,0x6a,0x6d, // R: Bass Drum (from VRC7)
    0x01,0x01,0x00,0x00,0xc8,0xd8,0xa7,0x68, // R: High-Hat(M) / Snare Drum(C) (from VRC7)
    0x05,0x01,0x00,0x00,0xf8,0xaa,0x59,0x55, // R: Tom-tom(M) / Top Cymbal(C) (from VRC7)
];

#[rustfmt::skip]
const EXP_TABLE: [u16; 256] = [
    0,    3,    6,    8,    11,   14,   17,   20,   22,   25,   28,   31,   34,   37,   40,   42,
    45,   48,   51,   54,   57,   60,   63,   66,   69,   72,   75,   78,   81,   84,   87,   90,
    93,   96,   99,   102,  105,  108,  111,  114,  117,  120,  123,  126,  130,  133,  136,  139,
    142,  145,  148,  152,  155,  158,  161,  164,  168,  171,  174,  177,  181,  184,  187,  190,
    194,  197,  200,  204,  207,  210,  214,  217,  220,  224,  227,  231,  234,  237,  241,  244,
    248,  251,  255,  258,  262,  265,  268,  272,  276,  279,  283,  286,  290,  293,  297,  300,
    304,  308,  311,  315,  318,  322,  326,  329,  333,  337,  340,  344,  348,  352,  355,  359,
    363,  367,  370,  374,  378,  382,  385,  389,  393,  397,  401,  405,  409,  412,  416,  420,
    424,  428,  432,  436,  440,  444,  448,  452,  456,  460,  464,  468,  472,  476,  480,  484,
    488,  492,  496,  501,  505,  509,  513,  517,  521,  526,  530,  534,  538,  542,  547,  551,
    555,  560,  564,  568,  572,  577,  581,  585,  590,  594,  599,  603,  607,  612,  616,  621,
    625,  630,  634,  639,  643,  648,  652,  657,  661,  666,  670,  675,  680,  684,  689,  693,
    698,  703,  708,  712,  717,  722,  726,  731,  736,  741,  745,  750,  755,  760,  765,  770,
    774,  779,  784,  789,  794,  799,  804,  809,  814,  819,  824,  829,  834,  839,  844,  849,
    854,  859,  864,  869,  874,  880,  885,  890,  895,  900,  906,  911,  916,  921,  927,  932,
    937,  942,  948,  953,  959,  964,  969,  975,  980,  986,  991,  996, 1002, 1007, 1013, 1018,
];

#[rustfmt::skip]
const FULLSIN_TABLE_QUARTER: [u16; PG_WIDTH / 4] = [
    2137, 1731, 1543, 1419, 1326, 1252, 1190, 1137, 1091, 1050, 1013, 979,  949,  920,  894,  869,
    846,  825,  804,  785,  767,  749,  732,  717,  701,  687,  672,  659,  646,  633,  621,  609,
    598,  587,  576,  566,  556,  546,  536,  527,  518,  509,  501,  492,  484,  476,  468,  461,
    453,  446,  439,  432,  425,  418,  411,  405,  399,  392,  386,  380,  375,  369,  363,  358,
    352,  347,  341,  336,  331,  326,  321,  316,  311,  307,  302,  297,  293,  289,  284,  280,
    276,  271,  267,  263,  259,  255,  251,  248,  244,  240,  236,  233,  229,  226,  222,  219,
    215,  212,  209,  205,  202,  199,  196,  193,  190,  187,  184,  181,  178,  175,  172,  169,
    167,  164,  161,  159,  156,  153,  151,  148,  146,  143,  141,  138,  136,  134,  131,  129,
    127,  125,  122,  120,  118,  116,  114,  112,  110,  108,  106,  104,  102,  100,  98,   96,
    94,   92,   91,   89,   87,   85,   83,   82,   80,   78,   77,   75,   74,   72,   70,   69,
    67,   66,   64,   63,   62,   60,   59,   57,   56,   55,   53,   52,   51,   49,   48,   47,
    46,   45,   43,   42,   41,   40,   39,   38,   37,   36,   35,   34,   33,   32,   31,   30,
    29,   28,   27,   26,   25,   24,   23,   23,   22,   21,   20,   20,   19,   18,   17,   17,
    16,   15,   15,   14,   13,   13,   12,   12,   11,   10,   10,   9,    9,    8,    8,    7,
    7,    7,    6,    6,    5,    5,    5,    4,    4,    4,    3,    3,    3,    2,    2,    2,
    2,    1,    1,    1,    1,    1,    1,    1,    0,    0,    0,    0,    0,    0,    0,    0,
];

#[rustfmt::skip]
const PM_TABLE: [[i8; 8]; 8] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 1, 0, 0, 0, -1, 0],
    [0, 1, 2, 1, 0, -1, -2, -1],
    [0, 1, 3, 1, 0, -1, -3, -1],
    [0, 2, 4, 2, 0, -2, -4, -2],
    [0, 2, 5, 2, 0, -2, -5, -2],
    [0, 3, 6, 3, 0, -3, -6, -3],
    [0, 3, 7, 3, 0, -3, -7, -3],
];

#[rustfmt::skip]
const AM_TABLE: [u8; 210] = [
    0,  0,  0,  0,  0,  0,  0,  0,  1,  1,  1,  1,  1,  1,  1,  1,
    2,  2,  2,  2,  2,  2,  2,  2,  3,  3,  3,  3,  3,  3,  3,  3,
    4,  4,  4,  4,  4,  4,  4,  4,  5,  5,  5,  5,  5,  5,  5,  5,
    6,  6,  6,  6,  6,  6,  6,  6,  7,  7,  7,  7,  7,  7,  7,  7,
    8,  8,  8,  8,  8,  8,  8,  8,  9,  9,  9,  9,  9,  9,  9,  9,
    10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11,
    12, 12, 12, 12, 12, 12, 12, 12,
    13, 13, 13,
    12, 12, 12, 12, 12, 12, 12, 12,
    11, 11, 11, 11, 11, 11, 11, 11, 10, 10, 10, 10, 10, 10, 10, 10,
    9,  9,  9,  9,  9,  9,  9,  9,  8,  8,  8,  8,  8,  8,  8,  8,
    7,  7,  7,  7,  7,  7,  7,  7,  6,  6,  6,  6,  6,  6,  6,  6,
    5,  5,  5,  5,  5,  5,  5,  5,  4,  4,  4,  4,  4,  4,  4,  4,
    3,  3,  3,  3,  3,  3,  3,  3,  2,  2,  2,  2,  2,  2,  2,  2,
    1,  1,  1,  1,  1,  1,  1,  1,  0,  0,  0,  0,  0,  0,  0,
];

const EG_STEP_TABLES: [[u8; 8]; 4] = [
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 0, 1, 1, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 1],
];

const ML_TABLE: [u32; 16] =
    [1, 1 * 2, 2 * 2, 3 * 2, 4 * 2, 5 * 2, 6 * 2, 7 * 2, 8 * 2, 9 * 2, 10 * 2, 10 * 2, 12 * 2, 12 * 2, 15 * 2, 15 * 2];

fn db2(x: f64) -> f64 {
    x * 2.0
}

#[rustfmt::skip]
const KL_TABLE: [f64; 16] = [
    0.0, 9.0, 12.0, 13.875, 15.0, 16.125, 16.875, 17.625, 18.0, 18.75, 19.125, 19.5, 19.875, 20.25, 20.625, 21.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgState {
    Attack,
    Decay,
    Sustain,
    Release,
    Damp,
}

#[derive(Clone, Copy, Default)]
struct Patch {
    tl: u32,
    fb: u32,
    eg: u32,
    ml: u32,
    ar: u32,
    dr: u32,
    sl: u32,
    rr: u32,
    kr: u32,
    kl: u32,
    am: u32,
    pm: u32,
    ws: u32,
}

const UPDATE_WS: u32 = 1;
const UPDATE_TLL: u32 = 2;
const UPDATE_RKS: u32 = 4;
const UPDATE_EG: u32 = 8;
const UPDATE_ALL: u32 = 255;

#[derive(Clone, Copy)]
struct Slot {
    typ: u8, // bit0: 0=modulator 1=carrier, bit1: single-slot (rhythm) mode
    patch_index: usize,
    output: [i32; 2],
    wave_table_half: bool, // false: fullsin (wave_table_map[0]), true: halfsin (map[1])
    pg_phase: u32,
    pg_out: u32,
    pg_keep: bool,
    blk_fnum: u16,
    fnum: u16,
    blk: u8,
    eg_state: EgState,
    volume: i32,
    key_flag: bool,
    sus_flag: bool,
    tll: u16,
    rks: u8,
    eg_rate_h: u8,
    eg_rate_l: u8,
    eg_shift: u32,
    eg_out: u16,
    update_requests: u32,
}

impl Slot {
    fn reset(number: usize) -> Self {
        Slot {
            typ: (number % 2) as u8,
            patch_index: 0,
            output: [0, 0],
            wave_table_half: false,
            pg_phase: 0,
            pg_out: 0,
            pg_keep: false,
            blk_fnum: 0,
            fnum: 0,
            blk: 0,
            eg_state: EgState::Release,
            volume: 0,
            key_flag: false,
            sus_flag: false,
            tll: 0,
            rks: 0,
            eg_rate_h: 0,
            eg_rate_l: 0,
            eg_shift: 0,
            eg_out: EG_MUTE,
            update_requests: 0,
        }
    }

    fn request_update(&mut self, flag: u32) {
        self.update_requests |= flag;
    }
}

use crate::chips::rateconv::RateConv;

pub struct Opll {
    clk: u32,
    rate: u32,

    adr: u32,

    // The chip synthesizes internally at a fixed clk/72 rate (~49.7kHz at typical YM2413
    // clocks); `calc()` steps that internal update however many times are needed to advance
    // by one *output* sample at `output_rate`, tracked via `time_acc` (matches emu2413's own
    // out_step/inp_step/out_time, renamed here since the original names don't actually say
    // which is the chip rate vs. the output rate).
    chip_rate: f64,
    output_rate: f64,
    time_acc: f64,

    reg: [u8; 0x40],
    test_flag: u8,
    slot_key_status: u32,
    rhythm_mode: bool,

    eg_counter: u32,

    pm_phase: u32,
    am_phase: i32,

    lfo_am: u8,

    noise: u32,
    short_noise: bool,

    patch_number: [i32; 9],
    slot: [Slot; 18],
    patch: [Patch; 19 * 2],

    mask: u32,

    ch_out: [i32; 14],
    mix_out: [i32; 2],

    conv: Option<RateConv>,

    fullsin_table: [u16; PG_WIDTH],
    halfsin_table: [u16; PG_WIDTH],
    tll_table: Vec<[[u32; 4]; 64]>, // indexed [blk_fnum >> 5][tl][kl], length 8*16=128
    rks_table: [[i32; 2]; 16],      // indexed [blk_fnum >> 8][kr]
}

fn make_sin_table() -> ([u16; PG_WIDTH], [u16; PG_WIDTH]) {
    let mut fullsin = [0u16; PG_WIDTH];
    for (x, v) in FULLSIN_TABLE_QUARTER.iter().enumerate() {
        fullsin[x] = *v;
    }
    for x in 0..PG_WIDTH / 4 {
        fullsin[PG_WIDTH / 4 + x] = fullsin[PG_WIDTH / 4 - x - 1];
    }
    for x in 0..PG_WIDTH / 2 {
        fullsin[PG_WIDTH / 2 + x] = 0x8000 | fullsin[x];
    }
    let mut halfsin = [0u16; PG_WIDTH];
    halfsin[..PG_WIDTH / 2].copy_from_slice(&fullsin[..PG_WIDTH / 2]);
    for x in PG_WIDTH / 2..PG_WIDTH {
        halfsin[x] = 0xfff;
    }
    (fullsin, halfsin)
}

fn make_tll_table() -> Vec<[[u32; 4]; 64]> {
    let mut table = vec![[[0u32; 4]; 64]; 8 * 16];
    for fnum in 0..16u32 {
        for block in 0..8u32 {
            for tl in 0..64u32 {
                for kl in 0..4u32 {
                    let idx = ((block << 4) | fnum) as usize;
                    if kl == 0 {
                        table[idx][tl as usize][kl as usize] = tl2eg(tl);
                    } else {
                        let tmp = KL_TABLE[fnum as usize] - db2(3.0) * (7 - block) as f64;
                        table[idx][tl as usize][kl as usize] = if tmp <= 0.0 {
                            tl2eg(tl)
                        } else {
                            (((tmp as i64) >> (3 - kl)) as f64 / EG_STEP) as u32 + tl2eg(tl)
                        };
                    }
                }
            }
        }
    }
    table
}

fn make_rks_table() -> [[i32; 2]; 16] {
    let mut table = [[0i32; 2]; 16];
    for fnum8 in 0..2i32 {
        for block in 0..8i32 {
            let idx = ((block << 1) | fnum8) as usize;
            table[idx][1] = (block << 1) + fnum8;
            table[idx][0] = block >> 1;
        }
    }
    table
}

fn dump_to_patch(dump: &[u8]) -> [Patch; 2] {
    let mut patch = [Patch::default(); 2];
    patch[0].am = ((dump[0] >> 7) & 1) as u32;
    patch[1].am = ((dump[1] >> 7) & 1) as u32;
    patch[0].pm = ((dump[0] >> 6) & 1) as u32;
    patch[1].pm = ((dump[1] >> 6) & 1) as u32;
    patch[0].eg = ((dump[0] >> 5) & 1) as u32;
    patch[1].eg = ((dump[1] >> 5) & 1) as u32;
    patch[0].kr = ((dump[0] >> 4) & 1) as u32;
    patch[1].kr = ((dump[1] >> 4) & 1) as u32;
    patch[0].ml = (dump[0] & 15) as u32;
    patch[1].ml = (dump[1] & 15) as u32;
    patch[0].kl = ((dump[2] >> 6) & 3) as u32;
    patch[1].kl = ((dump[3] >> 6) & 3) as u32;
    patch[0].tl = (dump[2] & 63) as u32;
    patch[1].tl = 0;
    patch[0].fb = (dump[3] & 7) as u32;
    patch[1].fb = 0;
    patch[0].ws = ((dump[3] >> 3) & 1) as u32;
    patch[1].ws = ((dump[3] >> 4) & 1) as u32;
    patch[0].ar = ((dump[4] >> 4) & 15) as u32;
    patch[1].ar = ((dump[5] >> 4) & 15) as u32;
    patch[0].dr = (dump[4] & 15) as u32;
    patch[1].dr = (dump[5] & 15) as u32;
    patch[0].sl = ((dump[6] >> 4) & 15) as u32;
    patch[1].sl = ((dump[7] >> 4) & 15) as u32;
    patch[0].rr = (dump[6] & 15) as u32;
    patch[1].rr = (dump[7] & 15) as u32;
    patch
}

fn get_default_patch(num: usize) -> [Patch; 2] {
    dump_to_patch(&DEFAULT_INST[num * 8..num * 8 + 8])
}

impl Opll {
    pub fn new(clk: u32, rate: u32) -> Self {
        let (fullsin_table, halfsin_table) = make_sin_table();
        let tll_table = make_tll_table();
        let rks_table = make_rks_table();

        let mut opll = Opll {
            clk,
            rate,
            adr: 0,
            chip_rate: 0.0,
            output_rate: 0.0,
            time_acc: 0.0,
            reg: [0; 0x40],
            test_flag: 0,
            slot_key_status: 0,
            rhythm_mode: false,
            eg_counter: 0,
            pm_phase: 0,
            am_phase: 0,
            lfo_am: 0,
            noise: 1,
            short_noise: false,
            patch_number: [0; 9],
            slot: std::array::from_fn(Slot::reset),
            patch: [Patch::default(); 19 * 2],
            mask: 0,
            ch_out: [0; 14],
            mix_out: [0, 0],
            conv: None,
            fullsin_table,
            halfsin_table,
            tll_table,
            rks_table,
        };
        opll.reset();
        opll.reset_patch();
        opll
    }

    fn reset_rate_conversion_params(&mut self) {
        let f_out = self.rate as f64;
        let f_inp = self.clk as f64 / 72.0;
        self.time_acc = 0.0;
        self.chip_rate = f_inp;
        self.output_rate = f_out;
        self.conv = None;
        if f_inp.floor() != f_out && (f_inp + 0.5).floor() != f_out {
            self.conv = Some(RateConv::new(f_inp, f_out));
        }
    }

    pub fn reset(&mut self) {
        self.adr = 0;
        self.pm_phase = 0;
        self.am_phase = 0;
        self.noise = 1;
        self.mask = 0;
        self.rhythm_mode = false;
        self.slot_key_status = 0;
        self.eg_counter = 0;

        self.reset_rate_conversion_params();

        for i in 0..18 {
            self.slot[i] = Slot::reset(i);
        }
        for i in 0..9 {
            self.set_patch(i, 0);
        }
        for i in 0..0x40 {
            self.write_reg(i, 0);
        }
        self.ch_out = [0; 14];
    }

    fn reset_patch(&mut self) {
        for i in 0..19 {
            let p = get_default_patch(i);
            self.patch[i * 2] = p[0];
            self.patch[i * 2 + 1] = p[1];
        }
    }

    fn mod_index(ch: usize) -> usize {
        ch << 1
    }
    fn car_index(ch: usize) -> usize {
        (ch << 1) | 1
    }

    fn set_patch(&mut self, ch: usize, num: i32) {
        self.patch_number[ch] = num;
        let mi = Self::mod_index(ch);
        let ci = Self::car_index(ch);
        self.slot[mi].patch_index = (num as usize) * 2;
        self.slot[ci].patch_index = (num as usize) * 2 + 1;
        self.slot[mi].request_update(UPDATE_ALL);
        self.slot[ci].request_update(UPDATE_ALL);
    }

    fn set_sus_flag(&mut self, ch: usize, flag: bool) {
        let ci = Self::car_index(ch);
        self.slot[ci].sus_flag = flag;
        self.slot[ci].request_update(UPDATE_EG);
        let mi = Self::mod_index(ch);
        if self.slot[mi].typ & 1 != 0 {
            self.slot[mi].sus_flag = flag;
            self.slot[mi].request_update(UPDATE_EG);
        }
    }

    fn set_volume(&mut self, ch: usize, volume: i32) {
        let ci = Self::car_index(ch);
        self.slot[ci].volume = volume;
        self.slot[ci].request_update(UPDATE_TLL);
    }

    fn set_slot_volume(&mut self, slot_index: usize, volume: i32) {
        self.slot[slot_index].volume = volume;
        self.slot[slot_index].request_update(UPDATE_TLL);
    }

    fn set_fnumber(&mut self, ch: usize, fnum: u16) {
        for idx in [Self::car_index(ch), Self::mod_index(ch)] {
            let s = &mut self.slot[idx];
            s.fnum = fnum;
            s.blk_fnum = (s.blk_fnum & 0xe00) | (fnum & 0x1ff);
            s.request_update(UPDATE_EG | UPDATE_RKS | UPDATE_TLL);
        }
    }

    fn set_block(&mut self, ch: usize, blk: u8) {
        for idx in [Self::car_index(ch), Self::mod_index(ch)] {
            let s = &mut self.slot[idx];
            s.blk = blk;
            s.blk_fnum = (((blk & 7) as u16) << 9) | (s.blk_fnum & 0x1ff);
            s.request_update(UPDATE_EG | UPDATE_RKS | UPDATE_TLL);
        }
    }

    fn update_rhythm_mode(&mut self) {
        let new_rhythm_mode = (self.reg[0x0e] >> 5) & 1 != 0;
        if self.rhythm_mode != new_rhythm_mode {
            if new_rhythm_mode {
                self.slot[SLOT_HH].typ = 3;
                self.slot[SLOT_HH].pg_keep = true;
                self.slot[SLOT_SD].typ = 3;
                self.slot[SLOT_TOM].typ = 3;
                self.slot[SLOT_CYM].typ = 3;
                self.slot[SLOT_CYM].pg_keep = true;
                self.set_patch(6, 16);
                self.set_patch(7, 17);
                self.set_patch(8, 18);
                let hh_vol = (((self.reg[0x37] >> 4) & 15) as i32) << 2;
                self.set_slot_volume(SLOT_HH, hh_vol);
                let tom_vol = (((self.reg[0x38] >> 4) & 15) as i32) << 2;
                self.set_slot_volume(SLOT_TOM, tom_vol);
            } else {
                self.slot[SLOT_HH].typ = 0;
                self.slot[SLOT_HH].pg_keep = false;
                self.slot[SLOT_SD].typ = 1;
                self.slot[SLOT_TOM].typ = 0;
                self.slot[SLOT_CYM].typ = 1;
                self.slot[SLOT_CYM].pg_keep = false;
                self.set_patch(6, (self.reg[0x36] >> 4) as i32);
                self.set_patch(7, (self.reg[0x37] >> 4) as i32);
                self.set_patch(8, (self.reg[0x38] >> 4) as i32);
            }
        }
        self.rhythm_mode = new_rhythm_mode;
    }

    fn slot_on(&mut self, i: usize) {
        self.slot[i].key_flag = true;
        self.slot[i].eg_state = EgState::Damp;
        self.slot[i].request_update(UPDATE_EG);
    }

    fn slot_off(&mut self, i: usize) {
        self.slot[i].key_flag = false;
        if self.slot[i].typ & 1 != 0 {
            self.slot[i].eg_state = EgState::Release;
            self.slot[i].request_update(UPDATE_EG);
        }
    }

    fn update_key_status(&mut self) {
        let r14 = self.reg[0x0e];
        let rhythm_mode = (r14 >> 5) & 1 != 0;
        let mut new_slot_key_status: u32 = 0;

        for ch in 0..9 {
            if self.reg[0x20 + ch] & 0x10 != 0 {
                new_slot_key_status |= 3 << (ch * 2);
            }
        }
        if rhythm_mode {
            if r14 & 0x10 != 0 {
                new_slot_key_status |= 3 << SLOT_BD1;
            }
            if r14 & 0x01 != 0 {
                new_slot_key_status |= 1 << SLOT_HH;
            }
            if r14 & 0x08 != 0 {
                new_slot_key_status |= 1 << SLOT_SD;
            }
            if r14 & 0x04 != 0 {
                new_slot_key_status |= 1 << SLOT_TOM;
            }
            if r14 & 0x02 != 0 {
                new_slot_key_status |= 1 << SLOT_CYM;
            }
        }

        let updated_status = self.slot_key_status ^ new_slot_key_status;
        if updated_status != 0 {
            for i in 0..18 {
                if (updated_status >> i) & 1 != 0 {
                    if (new_slot_key_status >> i) & 1 != 0 {
                        self.slot_on(i);
                    } else {
                        self.slot_off(i);
                    }
                }
            }
        }
        self.slot_key_status = new_slot_key_status;
    }

    pub fn write_reg(&mut self, reg: u32, data: u8) {
        if reg >= 0x40 {
            return;
        }
        let mut reg = reg;
        if (0x19..=0x1f).contains(&reg) || (0x29..=0x2f).contains(&reg) || (0x39..=0x3f).contains(&reg) {
            reg -= 9;
        }
        self.reg[reg as usize] = data;

        match reg {
            0x00 => {
                self.patch[0].am = ((data >> 7) & 1) as u32;
                self.patch[0].pm = ((data >> 6) & 1) as u32;
                self.patch[0].eg = ((data >> 5) & 1) as u32;
                self.patch[0].kr = ((data >> 4) & 1) as u32;
                self.patch[0].ml = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let mi = Self::mod_index(i);
                        self.slot[mi].request_update(UPDATE_RKS | UPDATE_EG);
                    }
                }
            }
            0x01 => {
                self.patch[1].am = ((data >> 7) & 1) as u32;
                self.patch[1].pm = ((data >> 6) & 1) as u32;
                self.patch[1].eg = ((data >> 5) & 1) as u32;
                self.patch[1].kr = ((data >> 4) & 1) as u32;
                self.patch[1].ml = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let ci = Self::car_index(i);
                        self.slot[ci].request_update(UPDATE_RKS | UPDATE_EG);
                    }
                }
            }
            0x02 => {
                self.patch[0].kl = ((data >> 6) & 3) as u32;
                self.patch[0].tl = (data & 63) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let mi = Self::mod_index(i);
                        self.slot[mi].request_update(UPDATE_TLL);
                    }
                }
            }
            0x03 => {
                self.patch[1].kl = ((data >> 6) & 3) as u32;
                self.patch[1].ws = ((data >> 4) & 1) as u32;
                self.patch[0].ws = ((data >> 3) & 1) as u32;
                self.patch[0].fb = (data & 7) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let mi = Self::mod_index(i);
                        let ci = Self::car_index(i);
                        self.slot[mi].request_update(UPDATE_WS);
                        self.slot[ci].request_update(UPDATE_WS | UPDATE_TLL);
                    }
                }
            }
            0x04 => {
                self.patch[0].ar = ((data >> 4) & 15) as u32;
                self.patch[0].dr = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let mi = Self::mod_index(i);
                        self.slot[mi].request_update(UPDATE_EG);
                    }
                }
            }
            0x05 => {
                self.patch[1].ar = ((data >> 4) & 15) as u32;
                self.patch[1].dr = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let ci = Self::car_index(i);
                        self.slot[ci].request_update(UPDATE_EG);
                    }
                }
            }
            0x06 => {
                self.patch[0].sl = ((data >> 4) & 15) as u32;
                self.patch[0].rr = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let mi = Self::mod_index(i);
                        self.slot[mi].request_update(UPDATE_EG);
                    }
                }
            }
            0x07 => {
                self.patch[1].sl = ((data >> 4) & 15) as u32;
                self.patch[1].rr = (data & 15) as u32;
                for i in 0..9 {
                    if self.patch_number[i] == 0 {
                        let ci = Self::car_index(i);
                        self.slot[ci].request_update(UPDATE_EG);
                    }
                }
            }
            0x0e => {
                self.update_rhythm_mode();
                self.update_key_status();
            }
            0x0f => {
                self.test_flag = data;
            }
            0x10..=0x18 => {
                let ch = (reg - 0x10) as usize;
                let fnum = data as u16 + (((self.reg[0x20 + ch] & 1) as u16) << 8);
                self.set_fnumber(ch, fnum);
            }
            0x20..=0x28 => {
                let ch = (reg - 0x20) as usize;
                let fnum = (((data & 1) as u16) << 8) + self.reg[0x10 + ch] as u16;
                self.set_fnumber(ch, fnum);
                self.set_block(ch, (data >> 1) & 7);
                self.set_sus_flag(ch, (data >> 5) & 1 != 0);
                self.update_key_status();
            }
            0x30..=0x38 => {
                if (self.reg[0x0e] & 32 != 0) && reg >= 0x36 {
                    match reg {
                        0x37 => {
                            let v = (((data >> 4) & 15) as i32) << 2;
                            self.set_slot_volume(Self::mod_index(7), v);
                        }
                        0x38 => {
                            let v = (((data >> 4) & 15) as i32) << 2;
                            self.set_slot_volume(Self::mod_index(8), v);
                        }
                        _ => {}
                    }
                } else {
                    self.set_patch(reg as usize - 0x30, ((data >> 4) & 15) as i32);
                }
                self.set_volume(reg as usize - 0x30, ((data & 15) as i32) << 2);
            }
            _ => {}
        }
    }

    fn get_parameter_rate(&self, i: usize) -> u32 {
        let slot = &self.slot[i];
        if (slot.typ & 1) == 0 && !slot.key_flag {
            return 0;
        }
        let patch = &self.patch[slot.patch_index];
        match slot.eg_state {
            EgState::Attack => patch.ar,
            EgState::Decay => patch.dr,
            EgState::Sustain => {
                if patch.eg != 0 {
                    0
                } else {
                    patch.rr
                }
            }
            EgState::Release => {
                if slot.sus_flag {
                    5
                } else if patch.eg != 0 {
                    patch.rr
                } else {
                    7
                }
            }
            EgState::Damp => DAMPER_RATE as u32,
        }
    }

    fn commit_slot_update(&mut self, i: usize) {
        if self.slot[i].update_requests & UPDATE_WS != 0 {
            self.slot[i].wave_table_half = self.patch[self.slot[i].patch_index].ws != 0;
        }
        if self.slot[i].update_requests & UPDATE_TLL != 0 {
            let patch_index = self.slot[i].patch_index;
            let blk_fnum = self.slot[i].blk_fnum;
            let kl = self.patch[patch_index].kl as usize;
            let idx = (blk_fnum >> 5) as usize;
            if (self.slot[i].typ & 1) == 0 {
                let tl = self.patch[patch_index].tl as usize;
                self.slot[i].tll = self.tll_table[idx][tl][kl] as u16;
            } else {
                let vol = self.slot[i].volume.clamp(0, 63) as usize;
                self.slot[i].tll = self.tll_table[idx][vol][kl] as u16;
            }
        }
        if self.slot[i].update_requests & UPDATE_RKS != 0 {
            let patch_index = self.slot[i].patch_index;
            let kr = self.patch[patch_index].kr as usize;
            let idx = (self.slot[i].blk_fnum >> 8) as usize;
            self.slot[i].rks = self.rks_table[idx][kr] as u8;
        }
        if self.slot[i].update_requests & (UPDATE_RKS | UPDATE_EG) != 0 {
            let p_rate = self.get_parameter_rate(i);
            let slot = &mut self.slot[i];
            if p_rate == 0 {
                slot.eg_shift = 0;
                slot.eg_rate_h = 0;
                slot.eg_rate_l = 0;
                slot.update_requests = 0;
                return;
            }
            slot.eg_rate_h = 15.min(p_rate + (slot.rks as u32 >> 2)) as u8;
            slot.eg_rate_l = slot.rks & 3;
            if slot.eg_state == EgState::Attack {
                slot.eg_shift = if slot.eg_rate_h > 0 && slot.eg_rate_h < 12 { 13 - slot.eg_rate_h as u32 } else { 0 };
            } else {
                slot.eg_shift = if slot.eg_rate_h < 13 { 13 - slot.eg_rate_h as u32 } else { 0 };
            }
        }
        self.slot[i].update_requests = 0;
    }

    fn start_envelope(&mut self, i: usize) {
        let patch = &self.patch[self.slot[i].patch_index];
        if 15.min(patch.ar + (self.slot[i].rks as u32 >> 2)) == 15 {
            self.slot[i].eg_state = EgState::Decay;
            self.slot[i].eg_out = 0;
        } else {
            self.slot[i].eg_state = EgState::Attack;
        }
        self.slot[i].request_update(UPDATE_EG);
    }

    fn lookup_attack_step(&self, i: usize, counter: u32) -> u8 {
        let slot = &self.slot[i];
        match slot.eg_rate_h {
            12 => {
                let index = ((counter & 0xc) >> 1) as usize;
                4 - EG_STEP_TABLES[slot.eg_rate_l as usize][index]
            }
            13 => {
                let index = ((counter & 0xc) >> 1) as usize;
                3 - EG_STEP_TABLES[slot.eg_rate_l as usize][index]
            }
            14 => {
                let index = ((counter & 0xc) >> 1) as usize;
                2 - EG_STEP_TABLES[slot.eg_rate_l as usize][index]
            }
            0 | 15 => 0,
            _ => {
                let index = ((counter >> slot.eg_shift) & 7) as usize;
                if EG_STEP_TABLES[slot.eg_rate_l as usize][index] != 0 {
                    4
                } else {
                    0
                }
            }
        }
    }

    fn lookup_decay_step(&self, i: usize, counter: u32) -> u8 {
        let slot = &self.slot[i];
        match slot.eg_rate_h {
            0 => 0,
            13 => {
                let index = (((counter & 0xc) >> 1) | (counter & 1)) as usize;
                EG_STEP_TABLES[slot.eg_rate_l as usize][index]
            }
            14 => {
                let index = ((counter & 0xc) >> 1) as usize;
                EG_STEP_TABLES[slot.eg_rate_l as usize][index] + 1
            }
            15 => 2,
            _ => {
                let index = ((counter >> slot.eg_shift) & 7) as usize;
                EG_STEP_TABLES[slot.eg_rate_l as usize][index]
            }
        }
    }

    /// Returns `Some(reset_pg_phase_for_this_slot)` request plus whether the buddy slot's
    /// phase should also reset, deferred to the caller since Rust can't hold `&mut` borrows
    /// on both this slot and its buddy simultaneously the way the C pointer-based version does.
    fn calc_envelope(&mut self, i: usize, eg_counter: u32, test: bool) -> (bool, bool) {
        let mask = (1u32 << self.slot[i].eg_shift) - 1;
        let mut reset_own_phase = false;
        let mut reset_buddy_phase = false;

        if self.slot[i].eg_state == EgState::Attack {
            if self.slot[i].eg_out > 0 && self.slot[i].eg_rate_h > 0 && (eg_counter & mask & !3) == 0 {
                let s = self.lookup_attack_step(i, eg_counter);
                if s > 0 {
                    let eg_out = self.slot[i].eg_out as i32;
                    self.slot[i].eg_out = 0.max(eg_out - (eg_out >> s) - 1) as u16;
                }
            }
        } else if self.slot[i].eg_rate_h > 0 && (eg_counter & mask) == 0 {
            let step = self.lookup_decay_step(i, eg_counter);
            self.slot[i].eg_out = EG_MUTE.min(self.slot[i].eg_out + step as u16);
        }

        match self.slot[i].eg_state {
            EgState::Damp => {
                if self.slot[i].eg_out >= EG_MAX && (eg_counter & mask) == 0 {
                    self.start_envelope(i);
                    if self.slot[i].typ & 1 != 0 {
                        if !self.slot[i].pg_keep {
                            reset_own_phase = true;
                        }
                        reset_buddy_phase = true; // caller checks the buddy's own pg_keep
                    }
                }
            }
            EgState::Attack => {
                if self.slot[i].eg_out == 0 {
                    self.slot[i].eg_state = EgState::Decay;
                    self.slot[i].request_update(UPDATE_EG);
                }
            }
            EgState::Decay => {
                let patch = &self.patch[self.slot[i].patch_index];
                if (self.slot[i].eg_out >> 3) as u32 == patch.sl {
                    self.slot[i].eg_state = EgState::Sustain;
                    self.slot[i].request_update(UPDATE_EG);
                }
            }
            EgState::Sustain | EgState::Release => {}
        }

        if test {
            self.slot[i].eg_out = 0;
        }

        if reset_own_phase {
            self.slot[i].pg_phase = 0;
        }
        (reset_own_phase, reset_buddy_phase)
    }

    fn calc_phase(&mut self, i: usize, pm_phase: u32, reset: bool) {
        let patch = self.patch[self.slot[i].patch_index];
        let slot = &mut self.slot[i];
        let pm = if patch.pm != 0 { PM_TABLE[((slot.fnum >> 6) & 7) as usize][((pm_phase >> 10) & 7) as usize] } else { 0 };
        if reset {
            slot.pg_phase = 0;
        }
        let delta = (((slot.fnum & 0x1ff) as i32 * 2 + pm as i32) as u32 * ML_TABLE[patch.ml as usize]) << slot.blk >> 2;
        slot.pg_phase = slot.pg_phase.wrapping_add(delta) & (DP_WIDTH - 1);
        slot.pg_out = slot.pg_phase >> DP_BASE_BITS;
    }

    fn update_slots(&mut self) {
        self.eg_counter += 1;
        for i in 0..18 {
            let buddy = if self.slot[i].typ == 0 {
                Some(i + 1)
            } else if self.slot[i].typ == 1 {
                Some(i - 1)
            } else {
                None
            };
            if self.slot[i].update_requests != 0 {
                self.commit_slot_update(i);
            }
            let test = self.test_flag & 1 != 0;
            let (_, reset_buddy) = self.calc_envelope(i, self.eg_counter, test);
            if reset_buddy {
                if let Some(b) = buddy {
                    if !self.slot[b].pg_keep {
                        self.slot[b].pg_phase = 0;
                    }
                }
            }
            let pm_phase = self.pm_phase;
            let phase_reset = self.test_flag & 4 != 0;
            self.calc_phase(i, pm_phase, phase_reset);
        }
    }

    fn update_ampm(&mut self) {
        if self.test_flag & 2 != 0 {
            self.pm_phase = 0;
            self.am_phase = 0;
        } else {
            self.pm_phase = self.pm_phase.wrapping_add(if self.test_flag & 8 != 0 { 1024 } else { 1 });
            self.am_phase = self.am_phase.wrapping_add(if self.test_flag & 8 != 0 { 64 } else { 1 });
        }
        let idx = ((self.am_phase >> 6) as usize) % AM_TABLE.len();
        self.lfo_am = AM_TABLE[idx];
    }

    fn update_noise(&mut self, cycle: u32) {
        for _ in 0..cycle {
            if self.noise & 1 != 0 {
                self.noise ^= 0x800200;
            }
            self.noise >>= 1;
        }
    }

    fn update_short_noise(&mut self) {
        let pg_hh = self.slot[SLOT_HH].pg_out;
        let pg_cym = self.slot[SLOT_CYM].pg_out;

        let h_bit2 = (pg_hh >> (PG_BITS - 8)) & 1;
        let h_bit7 = (pg_hh >> (PG_BITS - 3)) & 1;
        let h_bit3 = (pg_hh >> (PG_BITS - 7)) & 1;

        let c_bit3 = (pg_cym >> (PG_BITS - 7)) & 1;
        let c_bit5 = (pg_cym >> (PG_BITS - 5)) & 1;

        self.short_noise = ((h_bit2 ^ h_bit7) | (h_bit3 ^ c_bit5) | (c_bit3 ^ c_bit5)) != 0;
    }

    fn lookup_exp_table(i: u16) -> i16 {
        let t = EXP_TABLE[((i & 0xff) ^ 0xff) as usize] as i32 + 1024;
        let res = (t >> ((i & 0x7f00) >> 8)) as i16;
        (if i & 0x8000 != 0 { !res } else { res }) << 1
    }

    fn to_linear(&self, h: u16, slot_index: usize, am: u8) -> i16 {
        let slot = &self.slot[slot_index];
        if slot.eg_out > EG_MAX {
            return 0;
        }
        let att = (EG_MUTE.min(slot.eg_out + slot.tll + am as u16)) << 4;
        Self::lookup_exp_table(h.wrapping_add(att))
    }

    fn wave_table(&self, slot_index: usize) -> &[u16; PG_WIDTH] {
        if self.slot[slot_index].wave_table_half {
            &self.halfsin_table
        } else {
            &self.fullsin_table
        }
    }

    fn calc_slot_car(&mut self, ch: usize, fm: i32) -> i32 {
        let idx = Self::car_index(ch);
        let patch = self.patch[self.slot[idx].patch_index];
        let am = if patch.am != 0 { self.lfo_am } else { 0 };
        let pg_out = self.slot[idx].pg_out;
        let wt_idx = (pg_out as i32 + 2 * (fm >> 1)) as u32 & (PG_WIDTH as u32 - 1);
        let h = self.wave_table(idx)[wt_idx as usize];
        let out = self.to_linear(h, idx, am) as i32;
        self.slot[idx].output[1] = self.slot[idx].output[0];
        self.slot[idx].output[0] = out;
        out
    }

    fn calc_slot_mod(&mut self, ch: usize) -> i32 {
        let idx = Self::mod_index(ch);
        let patch = self.patch[self.slot[idx].patch_index];
        let fm = if patch.fb > 0 {
            (self.slot[idx].output[1] + self.slot[idx].output[0]) >> (9 - patch.fb)
        } else {
            0
        };
        let am = if patch.am != 0 { self.lfo_am } else { 0 };
        let pg_out = self.slot[idx].pg_out;
        let wt_idx = (pg_out as i32 + fm) as u32 & (PG_WIDTH as u32 - 1);
        let h = self.wave_table(idx)[wt_idx as usize];
        let out = self.to_linear(h, idx, am) as i32;
        self.slot[idx].output[1] = self.slot[idx].output[0];
        self.slot[idx].output[0] = out;
        out
    }

    fn calc_slot_tom(&mut self) -> i32 {
        let idx = Self::mod_index(8);
        let pg_out = self.slot[idx].pg_out as usize;
        let h = self.wave_table(idx)[pg_out];
        self.to_linear(h, idx, 0) as i32
    }

    fn pd(phase: u32) -> usize {
        // Specify phase offset directly based on a 10-bit (1024-length) sine table.
        if PG_BITS < 10 {
            (phase >> (10 - PG_BITS)) as usize
        } else {
            (phase << (PG_BITS - 10)) as usize
        }
    }

    fn calc_slot_snare(&mut self) -> i32 {
        let idx = Self::car_index(7);
        let pg_out = self.slot[idx].pg_out;
        let hi_bit = (pg_out >> (PG_BITS - 2)) & 1 != 0;
        let noise_bit = self.noise & 1 != 0;
        let phase = if hi_bit {
            if noise_bit { Self::pd(0x300) } else { Self::pd(0x200) }
        } else if noise_bit {
            Self::pd(0x0)
        } else {
            Self::pd(0x100)
        };
        let h = self.wave_table(idx)[phase];
        self.to_linear(h, idx, 0) as i32
    }

    fn calc_slot_cym(&mut self) -> i32 {
        let idx = Self::car_index(8);
        let phase = if self.short_noise { Self::pd(0x300) } else { Self::pd(0x100) };
        let h = self.wave_table(idx)[phase];
        self.to_linear(h, idx, 0) as i32
    }

    fn calc_slot_hat(&mut self) -> i32 {
        let idx = Self::mod_index(7);
        let noise_bit = self.noise & 1 != 0;
        let phase = if self.short_noise {
            if noise_bit { Self::pd(0x2d0) } else { Self::pd(0x234) }
        } else if noise_bit {
            Self::pd(0x34)
        } else {
            Self::pd(0xd0)
        };
        let h = self.wave_table(idx)[phase];
        self.to_linear(h, idx, 0) as i32
    }

    fn mo(x: i32) -> i32 {
        -x >> 1
    }
    fn ro(x: i32) -> i32 {
        x
    }

    const MASK_CH: fn(usize) -> u32 = |x| 1 << x;
    pub const MASK_HH: u32 = 1 << 9;
    pub const MASK_CYM: u32 = 1 << 10;
    pub const MASK_TOM: u32 = 1 << 11;
    pub const MASK_SD: u32 = 1 << 12;
    pub const MASK_BD: u32 = 1 << 13;

    fn update_output(&mut self) {
        self.update_ampm();
        self.update_short_noise();
        self.update_slots();

        for i in 0..6 {
            if self.mask & Self::MASK_CH(i) == 0 {
                let mod_out = self.calc_slot_mod(i);
                self.ch_out[i] = Self::mo(self.calc_slot_car(i, mod_out));
            }
        }

        if !self.rhythm_mode {
            if self.mask & Self::MASK_CH(6) == 0 {
                let mod_out = self.calc_slot_mod(6);
                self.ch_out[6] = Self::mo(self.calc_slot_car(6, mod_out));
            }
        } else if self.mask & Self::MASK_BD == 0 {
            let mod_out = self.calc_slot_mod(6);
            self.ch_out[9] = Self::ro(self.calc_slot_car(6, mod_out));
        }
        self.update_noise(14);

        if !self.rhythm_mode {
            if self.mask & Self::MASK_CH(7) == 0 {
                let mod_out = self.calc_slot_mod(7);
                self.ch_out[7] = Self::mo(self.calc_slot_car(7, mod_out));
            }
        } else {
            if self.mask & Self::MASK_HH == 0 {
                self.ch_out[10] = Self::ro(self.calc_slot_hat());
            }
            if self.mask & Self::MASK_SD == 0 {
                self.ch_out[11] = Self::ro(self.calc_slot_snare());
            }
        }
        self.update_noise(2);

        if !self.rhythm_mode {
            if self.mask & Self::MASK_CH(8) == 0 {
                let mod_out = self.calc_slot_mod(8);
                self.ch_out[8] = Self::mo(self.calc_slot_car(8, mod_out));
            }
        } else {
            if self.mask & Self::MASK_TOM == 0 {
                self.ch_out[12] = Self::ro(self.calc_slot_tom());
            }
            if self.mask & Self::MASK_CYM == 0 {
                self.ch_out[13] = Self::ro(self.calc_slot_cym());
            }
        }
        self.update_noise(2);
    }

    fn mix_output(&mut self) {
        let out: i32 = self.ch_out.iter().sum();
        let out = out.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        if let Some(conv) = &mut self.conv {
            conv.put_data(0, out);
        } else {
            self.mix_out[0] = out as i32;
        }
    }

    /// Mutes selected channels' contribution to the final mix — e.g. `mask_ch(2)` — without
    /// otherwise touching their simulated state, so isolating one channel for a stem render
    /// doesn't change how any other channel sounds (shared LFOs/noise keep running normally).
    /// Channels 0-8 are the 9 FM voices; MASK_BD/MASK_HH/MASK_SD/MASK_TOM/MASK_CYM are the
    /// rhythm-mode percussion voices that replace channels 6-8 when rhythm mode is enabled.
    pub fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
    }

    /// Mask bit for FM channel `ch` (0-8).
    pub fn mask_ch(ch: usize) -> u32 {
        Self::MASK_CH(ch)
    }

    /// Mask bit for every channel *except* the given one — i.e. what to pass to
    /// `set_mask` to isolate just that channel.
    pub fn solo_ch_mask(ch: usize) -> u32 {
        let all: u32 = (0..9).map(Self::MASK_CH).fold(0, |a, b| a | b) | Self::MASK_HH | Self::MASK_CYM | Self::MASK_TOM | Self::MASK_SD | Self::MASK_BD;
        all & !Self::MASK_CH(ch)
    }

    /// Mask bit for every rhythm voice except `hh`/`cym`/`tom`/`sd`/`bd` (pass the flag matching
    /// which one to keep audible), plus every FM channel — i.e. isolate one rhythm voice.
    pub fn solo_rhythm_mask(keep: u32) -> u32 {
        let all: u32 = (0..9).map(Self::MASK_CH).fold(0, |a, b| a | b) | Self::MASK_HH | Self::MASK_CYM | Self::MASK_TOM | Self::MASK_SD | Self::MASK_BD;
        all & !keep
    }

    /// Calculate one (mono) output sample at the configured output rate.
    pub fn calc(&mut self) -> i16 {
        while self.chip_rate > self.time_acc {
            self.time_acc += self.output_rate;
            self.update_output();
            self.mix_output();
        }
        self.time_acc -= self.chip_rate;
        if let Some(conv) = &mut self.conv {
            self.mix_out[0] = conv.get_data(0) as i32;
        }
        self.mix_out[0] as i16
    }
}
