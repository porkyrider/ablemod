//! Generic windowed-sinc sample-rate converter, ported from `OPLL_RateConv` in Mitsutaka
//! Okazaki's "emu2413" (MIT license) — extracted here because it's just as useful for
//! resampling chips::scc's native clock rate down to the target output rate as it is for
//! chips::ym2413's own clk/72 synthesis rate, and there's no reason to duplicate it.

pub(crate) struct RateConv {
    f_ratio: f64,
    timer: f64,
    sinc_table: Vec<i16>,
    buf: [[i16; Self::LW]; 2],
}

impl RateConv {
    const LW: usize = 16;
    const SINC_RESO: usize = 256;
    const SINC_AMP_BITS: i32 = 12;

    fn blackman(x: f64) -> f64 {
        0.42 - 0.5 * (2.0 * std::f64::consts::PI * x).cos() + 0.08 * (4.0 * std::f64::consts::PI * x).cos()
    }

    fn sinc(x: f64) -> f64 {
        if x == 0.0 {
            1.0
        } else {
            (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
        }
    }

    fn windowed_sinc(x: f64) -> f64 {
        Self::blackman(0.5 + 0.5 * x / (Self::LW as f64 / 2.0)) * Self::sinc(x)
    }

    pub(crate) fn new(f_inp: f64, f_out: f64) -> Self {
        let f_ratio = f_inp / f_out;
        let n = Self::SINC_RESO * Self::LW / 2;
        let mut sinc_table = vec![0i16; n];
        for (i, slot) in sinc_table.iter_mut().enumerate() {
            let x = i as f64 / Self::SINC_RESO as f64;
            *slot = if f_out < f_inp {
                ((1i32 << Self::SINC_AMP_BITS) as f64 * Self::windowed_sinc(x / f_ratio) / f_ratio) as i16
            } else {
                ((1i32 << Self::SINC_AMP_BITS) as f64 * Self::windowed_sinc(x)) as i16
            };
        }
        RateConv { f_ratio, timer: 0.0, sinc_table, buf: [[0; Self::LW]; 2] }
    }

    fn lookup_sinc_table(&self, x: f64) -> i16 {
        let mut index = (x * Self::SINC_RESO as f64) as i32;
        if index < 0 {
            index = -index;
        }
        let max_index = (Self::SINC_RESO * Self::LW / 2) as i32 - 1;
        self.sinc_table[index.min(max_index) as usize]
    }

    pub(crate) fn put_data(&mut self, ch: usize, data: i16) {
        let buf = &mut self.buf[ch];
        buf.copy_within(1.., 0);
        buf[Self::LW - 1] = data;
    }

    pub(crate) fn get_data(&mut self, ch: usize) -> i16 {
        self.timer += self.f_ratio;
        let dn = self.timer - self.timer.floor();
        self.timer = dn;

        let mut sum: i32 = 0;
        for k in 0..Self::LW {
            let x = (k as f64 - (Self::LW as f64 / 2.0 - 1.0)) - dn;
            sum += self.buf[ch][k] as i32 * self.lookup_sinc_table(x) as i32;
        }
        (sum >> Self::SINC_AMP_BITS) as i16
    }
}
