use std::f32::consts::PI;

#[derive(Debug, Clone, Copy)]
pub struct PsolaConfig {
    pub frame_size: usize,
    pub overlap: usize,
}

pub struct PsolaShifter {
    frame_size: usize,
    step_size: usize,
    latency: usize,
    sample_rate: f32,

    in_fifo: Vec<f32>,
    out_fifo: Vec<f32>,
    output_accum: Vec<f32>,
    gain_accum: Vec<f32>,

    psola_frame: Vec<f32>,
    weight_frame: Vec<f32>,
    ola_window: Vec<f32>,
    analysis_marks: Vec<usize>,
    synthesis_marks: Vec<usize>,
    max_marks: usize,
    period_smooth_coeff: f32,
    period_samples_smooth: f32,
    voiced_mix: f32,
    voiced_mix_coeff: f32,

    rover: usize,
}

impl PsolaShifter {
    pub fn new(sample_rate: f32, cfg: PsolaConfig) -> Self {
        let frame_size = cfg.frame_size.max(256).next_power_of_two();
        let overlap = cfg.overlap.max(2);
        let step_size = (frame_size / overlap).max(1);
        let latency = frame_size - step_size;

        let mut ola_window = vec![0.0; frame_size];
        for (i, w) in ola_window.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * (2.0 * PI * i as f32 / frame_size as f32).cos();
        }

        let transition_ms = 20.0;
        let tau_samples = transition_ms * 0.001 * sample_rate;
        let voiced_mix_coeff = (-(step_size as f32) / tau_samples.max(1.0)).exp();

        let period_smooth_ms = 15.0;
        let period_smooth_tau = period_smooth_ms * 0.001 * sample_rate;
        let period_smooth_coeff = (-(step_size as f32) / period_smooth_tau.max(1.0)).exp();

        let max_marks = (frame_size / 8).max(16);

        Self {
            frame_size,
            step_size,
            latency,
            sample_rate,
            in_fifo: vec![0.0; frame_size],
            out_fifo: vec![0.0; frame_size],
            output_accum: vec![0.0; frame_size * 2],
            gain_accum: vec![0.0; frame_size * 2],
            psola_frame: vec![0.0; frame_size],
            weight_frame: vec![0.0; frame_size],
            ola_window,
            analysis_marks: vec![0; max_marks],
            synthesis_marks: vec![0; max_marks],
            max_marks,
            period_smooth_coeff,
            period_samples_smooth: 0.0,
            voiced_mix: 1.0,
            voiced_mix_coeff,
            rover: latency,
        }
    }

    pub fn process_block(
        &mut self,
        pitch_ratio: f32,
        detected_pitch_hz: f32,
        input: &[f32],
        output: &mut [f32],
    ) {
        debug_assert_eq!(input.len(), output.len());
        let ratio = pitch_ratio.clamp(0.5, 2.0);

        for (in_sample, out_sample) in input.iter().copied().zip(output.iter_mut()) {
            self.in_fifo[self.rover] = in_sample;
            let y = self.out_fifo[self.rover - self.latency];
            *out_sample = if y.is_finite() { y } else { 0.0 };
            self.rover += 1;

            if self.rover >= self.frame_size {
                self.rover = self.latency;
                self.process_frame(ratio, detected_pitch_hz);
            }
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.latency
    }

    fn process_frame(&mut self, ratio: f32, detected_pitch_hz: f32) {
        self.psola_frame.fill(0.0);
        self.weight_frame.fill(0.0);

        let voiced_candidate = detected_pitch_hz.is_finite() && detected_pitch_hz >= 40.0;
        let zcr = zero_crossing_rate(&self.in_fifo);
        let voiced = voiced_candidate && zcr < 0.22;

        let target_mix = if voiced { 1.0 } else { 0.0 };
        self.voiced_mix =
            self.voiced_mix_coeff * self.voiced_mix + (1.0 - self.voiced_mix_coeff) * target_mix;

        if voiced {
            let pitch_hz = detected_pitch_hz;

            let period_target = (self.sample_rate / pitch_hz).clamp(24.0, (self.frame_size / 3) as f32);
            if self.period_samples_smooth <= 0.0 {
                self.period_samples_smooth = period_target;
            } else {
                self.period_samples_smooth = self.period_smooth_coeff * self.period_samples_smooth
                    + (1.0 - self.period_smooth_coeff) * period_target;
            }

            let period_a = self
                .period_samples_smooth
                .round()
                .clamp(24.0, (self.frame_size / 3) as f32) as usize;
            let period_s = ((period_a as f32) / ratio).round().max(1.0) as usize;
            let radius = period_a;

            let analysis_count = self.detect_pitch_marks(period_a);
            let synthesis_count = self.generate_synthesis_marks(period_s, radius);

            if analysis_count >= 2 && synthesis_count >= 1 {
                self.psola_overlap_grains(analysis_count, synthesis_count, radius);
            } else {
                // Gdy nie da sie wyznaczyc stabilnych pitch-markow,
                // przechodzimy do transparentnego passthrough dla zachowania transjentow.
                self.psola_frame.copy_from_slice(&self.in_fifo);
            }
        } else {
            // Brak wiarygodnego okresu: frame pozostaje zerowy i zostanie domiksowany sygnalem dry.
            self.psola_frame.fill(0.0);
        }

        // Crossfade voiced/unvoiced miedzy frame'ami:
        // twardy przeskok miedzy PSOLA i passthrough powoduje klikniecia i nieciaglosci fazy.
        // Plynna modulacja voiced_mix eliminuje ten artefakt.
        let wet = self.voiced_mix;
        let dry = 1.0 - wet;
        for i in 0..self.frame_size {
            self.psola_frame[i] = wet * self.psola_frame[i] + dry * self.in_fifo[i];
        }

        self.overlap_add_frame();
        self.shift_state();
    }

    fn detect_pitch_marks(&mut self, period_a: usize) -> usize {
        let search_radius = (period_a / 3).max(2);
        let min_pos = period_a;
        let max_pos = self.frame_size.saturating_sub(period_a + 1);

        if max_pos <= min_pos {
            return 0;
        }

        let center = self.frame_size / 2;
        let anchor_lo = center.saturating_sub(period_a).max(min_pos);
        let anchor_hi = (center + period_a).min(max_pos);
        let anchor = find_peak_abs(&self.in_fifo, anchor_lo, anchor_hi);

        let mut tmp_count = 0usize;
        self.analysis_marks[tmp_count] = anchor;
        tmp_count += 1;

        let mut expected = anchor;
        while expected > min_pos + period_a && tmp_count < self.max_marks {
            expected = expected.saturating_sub(period_a);
            let lo = expected.saturating_sub(search_radius).max(min_pos);
            let hi = (expected + search_radius).min(max_pos);
            let peak = find_peak_abs(&self.in_fifo, lo, hi);
            self.analysis_marks[tmp_count] = peak;
            tmp_count += 1;
        }

        expected = anchor;
        while expected + period_a < max_pos && tmp_count < self.max_marks {
            expected = expected.saturating_add(period_a);
            let lo = expected.saturating_sub(search_radius).max(min_pos);
            let hi = (expected + search_radius).min(max_pos);
            let peak = find_peak_abs(&self.in_fifo, lo, hi);
            self.analysis_marks[tmp_count] = peak;
            tmp_count += 1;
        }

        self.analysis_marks[..tmp_count].sort_unstable();
        tmp_count
    }

    fn generate_synthesis_marks(&mut self, period_s: usize, radius: usize) -> usize {
        let mut count = 0usize;
        let mut pos = radius;
        let max_pos = self.frame_size.saturating_sub(radius + 1);

        while pos < max_pos && count < self.max_marks {
            self.synthesis_marks[count] = pos;
            count += 1;
            pos = pos.saturating_add(period_s.max(1));
            if pos == usize::MAX {
                break;
            }
        }

        count
    }

    fn psola_overlap_grains(&mut self, analysis_count: usize, synthesis_count: usize, radius: usize) {
        let mut a_ptr = 0usize;
        for s_idx in 0..synthesis_count {
            let s_mark = self.synthesis_marks[s_idx];

            while a_ptr + 1 < analysis_count {
                let d0 = self.analysis_marks[a_ptr].abs_diff(s_mark);
                let d1 = self.analysis_marks[a_ptr + 1].abs_diff(s_mark);
                if d1 <= d0 {
                    a_ptr += 1;
                } else {
                    break;
                }
            }

            let a_idx = a_ptr;

            let ca = self.analysis_marks[a_idx] as isize;
            let cs = self.synthesis_marks[s_idx] as isize;

            for k in -(radius as isize)..=(radius as isize) {
                let si = ca + k;
                let so = cs + k;

                if si < 0 || so < 0 {
                    continue;
                }

                let si = si as usize;
                let so = so as usize;
                if si >= self.frame_size || so >= self.frame_size {
                    continue;
                }

                let x = (k as f32 + radius as f32) / (2.0 * radius as f32).max(1.0);
                let w = 0.5 - 0.5 * (2.0 * PI * x).cos();
                self.psola_frame[so] += self.in_fifo[si] * w;
                self.weight_frame[so] += w;
            }
        }

        for i in 0..self.frame_size {
            let denom = self.weight_frame[i];
            if denom > 1.0e-6 {
                self.psola_frame[i] /= denom;
            }
        }
    }

    fn overlap_add_frame(&mut self) {
        // Krytyczny punkt jakości: sama suma okienkowanych ramek bez kompensacji
        // zmienia obwiednie amplitudy w czasie. To daje modulacje ("boxy", "muffled"),
        // bo energia sygnalu pulsuje z czestotliwoscia hop-size.
        //
        // Poprawna praktyka produkcyjna: obok sumy sygnalu akumulujemy sume wag okna
        // i normalizujemy probki wyjsciowe przez lokalna sume okien.
        // Zapewnia to unity gain niezaleznie od overlapu i nieregularnosci grainow PSOLA.
        for i in 0..self.frame_size {
            let w = self.ola_window[i];
            self.output_accum[i] += self.psola_frame[i] * w;
            self.gain_accum[i] += w;
        }

        for i in 0..self.step_size {
            let g = self.gain_accum[i];
            self.out_fifo[i] = if g > 1.0e-6 {
                (self.output_accum[i] / g).clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    }

    fn shift_state(&mut self) {
        self.output_accum
            .copy_within(self.step_size..(self.step_size + self.frame_size), 0);
        self.output_accum[self.frame_size..].fill(0.0);
        self.gain_accum
            .copy_within(self.step_size..(self.step_size + self.frame_size), 0);
        self.gain_accum[self.frame_size..].fill(0.0);

        self.in_fifo.copy_within(self.step_size..self.frame_size, 0);
        self.in_fifo[(self.frame_size - self.step_size)..].fill(0.0);
    }
}

fn find_peak_abs(x: &[f32], start: usize, end: usize) -> usize {
    let mut idx = start;
    let mut max_v = x[start].abs();
    let mut i = start + 1;
    while i <= end {
        let v = x[i].abs();
        if v > max_v {
            max_v = v;
            idx = i;
        }
        i += 1;
    }
    idx
}

fn zero_crossing_rate(x: &[f32]) -> f32 {
    if x.len() < 2 {
        return 0.0;
    }
    let mut crossings = 0usize;
    for i in 1..x.len() {
        let a = x[i - 1];
        let b = x[i];
        if (a >= 0.0 && b < 0.0) || (a < 0.0 && b >= 0.0) {
            crossings += 1;
        }
    }
    crossings as f32 / (x.len() - 1) as f32
}
