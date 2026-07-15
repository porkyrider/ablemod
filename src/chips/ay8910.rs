//! AY-3-8910 / YM2149 (PSG) emulator — a faithful Rust port of Peter Sovietov's "ayumi"
//! (<https://github.com/true-grue/ayumi>, MIT license), chosen for its accuracy: it models
//! the chip's actual non-linear DAC output table (not a naive linear volume scale) and
//! oversamples+decimates through a proper windowed-sinc FIR filter rather than naively
//! sampling the raw square wave, which would alias badly.
//!
//! Kept close to the original C structure/names (tone/noise/envelope generators tick at the
//! chip's own internal rate; `process()` advances by one *output* sample) so it stays easy to
//! diff against the reference if a correctness question ever comes up.

const TONE_CHANNELS: usize = 3;
const DECIMATE_FACTOR: usize = 8;
const FIR_SIZE: usize = 192;
const DC_FILTER_SIZE: usize = 1024;

const AY_DAC_TABLE: [f64; 32] = [
    0.0, 0.0, 0.00999465934234, 0.00999465934234, 0.0144502937362, 0.0144502937362, 0.0210574502174,
    0.0210574502174, 0.0307011520562, 0.0307011520562, 0.0455481803616, 0.0455481803616, 0.0644998855573,
    0.0644998855573, 0.107362478065, 0.107362478065, 0.126588845655, 0.126588845655, 0.20498970016,
    0.20498970016, 0.292210269322, 0.292210269322, 0.372838941024, 0.372838941024, 0.492530708782,
    0.492530708782, 0.635324635691, 0.635324635691, 0.805584802014, 0.805584802014, 1.0, 1.0,
];

const YM_DAC_TABLE: [f64; 32] = [
    0.0, 0.0, 0.00465400167849, 0.00772106507973, 0.0109559777218, 0.0139620050355, 0.0169985503929,
    0.0200198367285, 0.024368657969, 0.029694056611, 0.0350652323186, 0.0403906309606, 0.0485389486534,
    0.0583352407111, 0.0680552376593, 0.0777752346075, 0.0925154497597, 0.111085679408, 0.129747463188,
    0.148485542077, 0.17666895552, 0.211551079576, 0.246387426566, 0.281101701381, 0.333730067903,
    0.400427252613, 0.467383840696, 0.53443198291, 0.635172045472, 0.75800717174, 0.879926756695, 1.0,
];

// The original C table holds function *pointers* per (shape, segment) and later compares
// them by address to decide the reset value (`== slide_down || == hold_top` -> 31, else 0).
// hold_top/hold_bottom are both true no-ops in C too, but distinct *symbols* there, so
// pointer comparison is safe under a C compiler's guarantees. Rust explicitly does not
// guarantee distinct addresses for functions with identical bodies (they can be folded
// together under optimization), so comparing fn pointers here would be a real, silent
// correctness bug — an enum makes "which segment behavior" a value we can match on instead.
#[derive(Clone, Copy, PartialEq)]
enum EnvelopeSegment {
    SlideUp,
    SlideDown,
    HoldTop,
    HoldBottom,
}
use EnvelopeSegment::{HoldBottom, HoldTop, SlideDown, SlideUp};

fn run_segment(ay: &mut Ayumi, segment: EnvelopeSegment) {
    match segment {
        SlideUp => {
            ay.envelope += 1;
            if ay.envelope > 31 {
                ay.envelope_segment ^= 1;
                reset_segment(ay);
            }
        }
        SlideDown => {
            ay.envelope -= 1;
            if ay.envelope < 0 {
                ay.envelope_segment ^= 1;
                reset_segment(ay);
            }
        }
        HoldTop | HoldBottom => {}
    }
}

#[rustfmt::skip]
const ENVELOPES: [[EnvelopeSegment; 2]; 16] = [
    [SlideDown, HoldBottom],
    [SlideDown, HoldBottom],
    [SlideDown, HoldBottom],
    [SlideDown, HoldBottom],
    [SlideUp, HoldBottom],
    [SlideUp, HoldBottom],
    [SlideUp, HoldBottom],
    [SlideUp, HoldBottom],
    [SlideDown, SlideDown],
    [SlideDown, HoldBottom],
    [SlideDown, SlideUp],
    [SlideDown, HoldTop],
    [SlideUp, SlideUp],
    [SlideUp, HoldTop],
    [SlideUp, SlideDown],
    [SlideUp, HoldBottom],
];

fn reset_segment(ay: &mut Ayumi) {
    let segment = ENVELOPES[ay.envelope_shape as usize][ay.envelope_segment as usize];
    if segment == SlideDown || segment == HoldTop {
        ay.envelope = 31;
    } else {
        ay.envelope = 0;
    }
}

#[derive(Clone, Copy, Default)]
struct ToneChannel {
    tone_period: i32,
    tone_counter: i32,
    tone: i32,
    t_off: i32,
    n_off: i32,
    e_on: i32,
    volume: i32,
    pan_left: f64,
    pan_right: f64,
}

#[derive(Clone, Copy, Default)]
struct Interpolator {
    c: [f64; 4],
    y: [f64; 4],
}

struct DcFilter {
    sum: f64,
    delay: [f64; DC_FILTER_SIZE],
}

impl Default for DcFilter {
    fn default() -> Self {
        DcFilter { sum: 0.0, delay: [0.0; DC_FILTER_SIZE] }
    }
}

pub struct Ayumi {
    channels: [ToneChannel; TONE_CHANNELS],
    noise_period: i32,
    noise_counter: i32,
    noise: u32,
    envelope_counter: i32,
    envelope_period: i32,
    envelope_shape: i32,
    envelope_segment: i32,
    envelope: i32,
    dac_table: &'static [f64; 32],
    step: f64,
    x: f64,
    interpolator_left: Interpolator,
    interpolator_right: Interpolator,
    fir_left: [f64; FIR_SIZE * 2],
    fir_right: [f64; FIR_SIZE * 2],
    fir_index: usize,
    dc_left: DcFilter,
    dc_right: DcFilter,
    dc_index: usize,
    /// Mutes a channel's contribution to the final mix without touching its simulated state
    /// (tone/noise/envelope counters keep ticking normally) — for isolating one channel at a
    /// time to render stems, not a real chip feature.
    mute: [bool; TONE_CHANNELS],
    pub left: f64,
    pub right: f64,
}

impl Ayumi {
    /// `is_ym`: true for YM2149's DAC table (Sega/MSX-era chips typically identify as this
    /// variant), false for the plain AY-3-8910 table. `clock_rate` is the chip's own input
    /// clock in Hz (VGM header value, unrelated to the emulator's own /8 internal division);
    /// `sample_rate` is the desired output sample rate.
    pub fn new(is_ym: bool, clock_rate: f64, sample_rate: u32) -> Self {
        let mut ay = Ayumi {
            channels: [ToneChannel::default(); TONE_CHANNELS],
            noise_period: 0,
            noise_counter: 0,
            noise: 1,
            envelope_counter: 0,
            envelope_period: 0,
            envelope_shape: 0,
            envelope_segment: 0,
            envelope: 0,
            dac_table: if is_ym { &YM_DAC_TABLE } else { &AY_DAC_TABLE },
            step: clock_rate / (sample_rate as f64 * 8.0 * DECIMATE_FACTOR as f64),
            x: 0.0,
            interpolator_left: Interpolator::default(),
            interpolator_right: Interpolator::default(),
            fir_left: [0.0; FIR_SIZE * 2],
            fir_right: [0.0; FIR_SIZE * 2],
            fir_index: 0,
            dc_left: DcFilter::default(),
            dc_right: DcFilter::default(),
            dc_index: 0,
            mute: [false; TONE_CHANNELS],
            left: 0.0,
            right: 0.0,
        };
        for ch in &mut ay.channels {
            ch.pan_left = 1.0;
            ch.pan_right = 1.0;
        }
        ay.set_envelope(1);
        for i in 0..TONE_CHANNELS {
            ay.set_tone(i, 1);
        }
        ay
    }

    /// Equal-power (`is_eqp=true`) or linear pan, `pan` in 0.0 (left) .. 1.0 (right).
    pub fn set_pan(&mut self, index: usize, pan: f64, is_eqp: bool) {
        if is_eqp {
            self.channels[index].pan_left = (1.0 - pan).sqrt();
            self.channels[index].pan_right = pan.sqrt();
        } else {
            self.channels[index].pan_left = 1.0 - pan;
            self.channels[index].pan_right = pan;
        }
    }

    pub fn set_tone(&mut self, index: usize, period: i32) {
        let period = period & 0xfff;
        self.channels[index].tone_period = ((period == 0) as i32) | period;
    }

    pub fn set_noise(&mut self, period: i32) {
        let period = period & 0x1f;
        self.noise_period = ((period == 0) as i32) | period;
    }

    pub fn set_mixer(&mut self, index: usize, t_off: i32, n_off: i32, e_on: i32) {
        self.channels[index].t_off = t_off & 1;
        self.channels[index].n_off = n_off & 1;
        self.channels[index].e_on = e_on;
    }

    /// Isolates one channel for a stem render — every *other* channel keeps ticking its tone/
    /// noise/envelope state normally (needed since noise/envelope are shared generators), it
    /// just stops contributing to the output mix.
    pub fn solo(&mut self, index: usize) {
        self.mute = [true; TONE_CHANNELS];
        self.mute[index] = false;
    }

    pub fn unmute_all(&mut self) {
        self.mute = [false; TONE_CHANNELS];
    }

    pub fn mute_all(&mut self) {
        self.mute = [true; TONE_CHANNELS];
    }

    pub fn set_volume(&mut self, index: usize, volume: i32) {
        self.channels[index].volume = volume & 0xf;
    }

    pub fn set_envelope(&mut self, period: i32) {
        let period = period & 0xffff;
        self.envelope_period = ((period == 0) as i32) | period;
    }

    pub fn set_envelope_shape(&mut self, shape: i32) {
        self.envelope_shape = shape & 0xf;
        self.envelope_counter = 0;
        self.envelope_segment = 0;
        reset_segment(self);
    }

    fn update_tone(&mut self, index: usize) -> i32 {
        let ch = &mut self.channels[index];
        ch.tone_counter += 1;
        if ch.tone_counter >= ch.tone_period {
            ch.tone_counter = 0;
            ch.tone ^= 1;
        }
        ch.tone
    }

    fn update_noise(&mut self) -> u32 {
        self.noise_counter += 1;
        if self.noise_counter >= self.noise_period << 1 {
            self.noise_counter = 0;
            let bit0x3 = (self.noise ^ (self.noise >> 3)) & 1;
            self.noise = (self.noise >> 1) | (bit0x3 << 16);
        }
        self.noise & 1
    }

    fn update_envelope(&mut self) -> i32 {
        self.envelope_counter += 1;
        if self.envelope_counter >= self.envelope_period {
            self.envelope_counter = 0;
            let segment = ENVELOPES[self.envelope_shape as usize][self.envelope_segment as usize];
            run_segment(self, segment);
        }
        self.envelope
    }

    fn update_mixer(&mut self) {
        let noise = self.update_noise() as i32;
        let envelope = self.update_envelope();
        self.left = 0.0;
        self.right = 0.0;
        for i in 0..TONE_CHANNELS {
            let tone = self.update_tone(i); // always ticks, even when muted, for correct state
            if self.mute[i] {
                continue;
            }
            let ch = &self.channels[i];
            let mut out = (tone | ch.t_off) & (noise | ch.n_off);
            out *= if ch.e_on != 0 { envelope } else { ch.volume * 2 + 1 };
            self.left += self.dac_table[out as usize] * ch.pan_left;
            self.right += self.dac_table[out as usize] * ch.pan_right;
        }
    }

    #[rustfmt::skip]
    fn decimate(x: &[f64]) -> f64 {
        -0.0000046183113992051936 * (x[1] + x[191])
            + -0.00001117761640887225 * (x[2] + x[190])
            + -0.000018610264502005432 * (x[3] + x[189])
            + -0.000025134586135631012 * (x[4] + x[188])
            + -0.000028494281690666197 * (x[5] + x[187])
            + -0.000026396828793275159 * (x[6] + x[186])
            + -0.000017094212558802156 * (x[7] + x[185])
            + 0.000023798193576966866 * (x[9] + x[183])
            + 0.000051281160242202183 * (x[10] + x[182])
            + 0.00007762197826243427 * (x[11] + x[181])
            + 0.000096759426664120416 * (x[12] + x[180])
            + 0.00010240229300393402 * (x[13] + x[179])
            + 0.000089344614218077106 * (x[14] + x[178])
            + 0.000054875700118949183 * (x[15] + x[177])
            + -0.000069839082210680165 * (x[17] + x[175])
            + -0.0001447966132360757 * (x[18] + x[174])
            + -0.00021158452917708308 * (x[19] + x[173])
            + -0.00025535069106550544 * (x[20] + x[172])
            + -0.00026228714374322104 * (x[21] + x[171])
            + -0.00022258805927027799 * (x[22] + x[170])
            + -0.00013323230495695704 * (x[23] + x[169])
            + 0.00016182578767055206 * (x[25] + x[167])
            + 0.00032846175385096581 * (x[26] + x[166])
            + 0.00047045611576184863 * (x[27] + x[165])
            + 0.00055713851457530944 * (x[28] + x[164])
            + 0.00056212565121518726 * (x[29] + x[163])
            + 0.00046901918553962478 * (x[30] + x[162])
            + 0.00027624866838952986 * (x[31] + x[161])
            + -0.00032564179486838622 * (x[33] + x[159])
            + -0.00065182310286710388 * (x[34] + x[158])
            + -0.00092127787309319298 * (x[35] + x[157])
            + -0.0010772534348943575 * (x[36] + x[156])
            + -0.0010737727700273478 * (x[37] + x[155])
            + -0.00088556645390392634 * (x[38] + x[154])
            + -0.00051581896090765534 * (x[39] + x[153])
            + 0.00059548767193795277 * (x[41] + x[151])
            + 0.0011803558710661009 * (x[42] + x[150])
            + 0.0016527320270369871 * (x[43] + x[149])
            + 0.0019152679330965555 * (x[44] + x[148])
            + 0.0018927324805381538 * (x[45] + x[147])
            + 0.0015481870327877937 * (x[46] + x[146])
            + 0.00089470695834941306 * (x[47] + x[145])
            + -0.0010178225878206125 * (x[49] + x[143])
            + -0.0020037400552054292 * (x[50] + x[142])
            + -0.0027874356824117317 * (x[51] + x[141])
            + -0.003210329988021943 * (x[52] + x[140])
            + -0.0031540624117984395 * (x[53] + x[139])
            + -0.0025657163651900345 * (x[54] + x[138])
            + -0.0014750752642111449 * (x[55] + x[137])
            + 0.0016624165446378462 * (x[57] + x[135])
            + 0.0032591192839069179 * (x[58] + x[134])
            + 0.0045165685815867747 * (x[59] + x[133])
            + 0.0051838984346123896 * (x[60] + x[132])
            + 0.0050774264697459933 * (x[61] + x[131])
            + 0.0041192521414141585 * (x[62] + x[130])
            + 0.0023628575417966491 * (x[63] + x[129])
            + -0.0026543507866759182 * (x[65] + x[127])
            + -0.0051990251084333425 * (x[66] + x[126])
            + -0.0072020238234656924 * (x[67] + x[125])
            + -0.0082672928192007358 * (x[68] + x[124])
            + -0.0081033739572956287 * (x[69] + x[123])
            + -0.006583111539570221 * (x[70] + x[122])
            + -0.0037839040415292386 * (x[71] + x[121])
            + 0.0042781252851152507 * (x[73] + x[119])
            + 0.0084176358598320178 * (x[74] + x[118])
            + 0.01172566057463055 * (x[75] + x[117])
            + 0.013550476647788672 * (x[76] + x[116])
            + 0.013388189369997496 * (x[77] + x[115])
            + 0.010979501242341259 * (x[78] + x[114])
            + 0.006381274941685413 * (x[79] + x[113])
            + -0.007421229604153888 * (x[81] + x[111])
            + -0.01486456304340213 * (x[82] + x[110])
            + -0.021143584622178104 * (x[83] + x[109])
            + -0.02504275058758609 * (x[84] + x[108])
            + -0.025473530942547201 * (x[85] + x[107])
            + -0.021627310017882196 * (x[86] + x[106])
            + -0.013104323383225543 * (x[87] + x[105])
            + 0.017065133989980476 * (x[89] + x[103])
            + 0.036978919264451952 * (x[90] + x[102])
            + 0.05823318062093958 * (x[91] + x[101])
            + 0.079072012081405949 * (x[92] + x[100])
            + 0.097675998716952317 * (x[93] + x[99])
            + 0.11236045936950932 * (x[94] + x[98])
            + 0.12176343577287731 * (x[95] + x[97])
            + 0.125 * x[96]
    }

    /// Advances by one *output* sample (internally ticks the chip DECIMATE_FACTOR times at
    /// its own oversampled rate and low-pass-filters back down) — call once per output frame,
    /// then read `.left`/`.right`.
    pub fn process(&mut self) {
        let fir_index = self.fir_index;
        self.fir_index = (self.fir_index + 1) % (FIR_SIZE / DECIMATE_FACTOR - 1);

        // mirrors the C pointer arithmetic `&ay->fir_left[FIR_SIZE - fir_index * DECIMATE_FACTOR]`
        let base = FIR_SIZE - fir_index * DECIMATE_FACTOR;

        for i in (0..DECIMATE_FACTOR).rev() {
            self.x += self.step;
            if self.x >= 1.0 {
                self.x -= 1.0;
                {
                    let y = &mut self.interpolator_left.y;
                    y[0] = y[1];
                    y[1] = y[2];
                    y[2] = y[3];
                }
                {
                    let y = &mut self.interpolator_right.y;
                    y[0] = y[1];
                    y[1] = y[2];
                    y[2] = y[3];
                }
                self.update_mixer();
                self.interpolator_left.y[3] = self.left;
                self.interpolator_right.y[3] = self.right;

                let y_left = self.interpolator_left.y;
                let y1 = y_left[2] - y_left[0];
                self.interpolator_left.c[0] = 0.5 * y_left[1] + 0.25 * (y_left[0] + y_left[2]);
                self.interpolator_left.c[1] = 0.5 * y1;
                self.interpolator_left.c[2] = 0.25 * (y_left[3] - y_left[1] - y1);

                let y_right = self.interpolator_right.y;
                let y1 = y_right[2] - y_right[0];
                self.interpolator_right.c[0] = 0.5 * y_right[1] + 0.25 * (y_right[0] + y_right[2]);
                self.interpolator_right.c[1] = 0.5 * y1;
                self.interpolator_right.c[2] = 0.25 * (y_right[3] - y_right[1] - y1);
            }
            let c_left = self.interpolator_left.c;
            let c_right = self.interpolator_right.c;
            self.fir_left[base + i] = (c_left[2] * self.x + c_left[1]) * self.x + c_left[0];
            self.fir_right[base + i] = (c_right[2] * self.x + c_right[1]) * self.x + c_right[0];
        }
        self.left = Self::decimate(&self.fir_left[base..base + FIR_SIZE]);
        self.right = Self::decimate(&self.fir_right[base..base + FIR_SIZE]);
        // the C code's decimate() ends with `memcpy(&x[FIR_SIZE - DECIMATE_FACTOR], x,
        // DECIMATE_FACTOR * sizeof(double))` — copying this window's *first* 8 entries
        // (relative to `base`, i.e. buf[base..base+8]) to its *last* 8 (buf[base+184..
        // base+192]), carrying the freshest samples forward for the next window to read as
        // history. Done here (after, not inside, decimate()) since Rust won't let a function
        // both borrow a slice immutably to read it and mutate the same backing array through
        // that borrow.
        for buf in [&mut self.fir_left, &mut self.fir_right] {
            let mut carry = [0.0; DECIMATE_FACTOR];
            carry.copy_from_slice(&buf[base..base + DECIMATE_FACTOR]);
            buf[base + FIR_SIZE - DECIMATE_FACTOR..base + FIR_SIZE].copy_from_slice(&carry);
        }
    }

    fn dc_filter_one(dc: &mut DcFilter, index: usize, x: f64) -> f64 {
        dc.sum += -dc.delay[index] + x;
        dc.delay[index] = x;
        x - dc.sum / DC_FILTER_SIZE as f64
    }

    pub fn remove_dc(&mut self) {
        self.left = Self::dc_filter_one(&mut self.dc_left, self.dc_index, self.left);
        self.right = Self::dc_filter_one(&mut self.dc_right, self.dc_index, self.right);
        self.dc_index = (self.dc_index + 1) & (DC_FILTER_SIZE - 1);
    }
}
