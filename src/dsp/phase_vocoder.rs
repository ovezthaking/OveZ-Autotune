use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

#[derive(Debug, Clone, Copy)]
pub struct PitchShiftConfig {
    pub frame_size: usize,
    pub oversampling: usize,
}

pub struct PhaseVocoderShifter {
    frame_size: usize,
    frame_size_f32: f32,
    oversampling: usize,
    step_size: usize,
    latency: usize,
    freq_per_bin: f32,
    expct: f32,

    window: Vec<f32>,
    in_fifo: Vec<f32>,
    out_fifo: Vec<f32>,
    fft_workspace: Vec<Complex32>,
    output_accum: Vec<f32>,
    last_phase: Vec<f32>,
    sum_phase: Vec<f32>,
    ana_magn: Vec<f32>,
    ana_freq: Vec<f32>,
    syn_magn: Vec<f32>,
    syn_freq: Vec<f32>,

    rover: usize,

    fft_forward: Arc<dyn Fft<f32>>,
    fft_inverse: Arc<dyn Fft<f32>>,
}

impl PhaseVocoderShifter {
    pub fn new(sample_rate: f32, cfg: PitchShiftConfig) -> Self {
        let frame_size = cfg.frame_size;
        let oversampling = cfg.oversampling.max(2);
        let step_size = frame_size / oversampling;
        let latency = frame_size - step_size;

        let mut planner = FftPlanner::<f32>::new();
        let fft_forward = planner.plan_fft_forward(frame_size);
        let fft_inverse = planner.plan_fft_inverse(frame_size);

        let mut window = vec![0.0; frame_size];
        for (i, w) in window.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * (2.0 * PI * i as f32 / frame_size as f32).cos();
        }

        let bins = frame_size / 2 + 1;

        Self {
            frame_size,
            frame_size_f32: frame_size as f32,
            oversampling,
            step_size,
            latency,
            freq_per_bin: sample_rate / frame_size as f32,
            expct: 2.0 * PI * step_size as f32 / frame_size as f32,
            window,
            in_fifo: vec![0.0; frame_size],
            out_fifo: vec![0.0; frame_size],
            fft_workspace: vec![Complex32::default(); frame_size],
            output_accum: vec![0.0; frame_size * 2],
            last_phase: vec![0.0; bins],
            sum_phase: vec![0.0; bins],
            ana_magn: vec![0.0; bins],
            ana_freq: vec![0.0; bins],
            syn_magn: vec![0.0; bins],
            syn_freq: vec![0.0; bins],
            rover: latency,
            fft_forward,
            fft_inverse,
        }
    }

    pub fn process_block(&mut self, pitch_ratio: f32, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        let ratio = pitch_ratio.clamp(0.5, 2.0);

        for (in_sample, out_sample) in input.iter().copied().zip(output.iter_mut()) {
            self.in_fifo[self.rover] = in_sample;
            *out_sample = self.out_fifo[self.rover - self.latency];
            self.rover += 1;

            if self.rover >= self.frame_size {
                self.rover = self.latency;
                self.process_frame(ratio);
            }
        }
    }

    fn process_frame(&mut self, ratio: f32) {
        for k in 0..self.frame_size {
            self.fft_workspace[k] = Complex32::new(self.in_fifo[k] * self.window[k], 0.0);
        }

        self.fft_forward.process(&mut self.fft_workspace);

        for k in 0..self.ana_magn.len() {
            let c = self.fft_workspace[k];
            let magn = 2.0 * (c.re * c.re + c.im * c.im).sqrt();
            let phase = c.im.atan2(c.re);

            let mut delta_phase = phase - self.last_phase[k];
            self.last_phase[k] = phase;

            delta_phase -= k as f32 * self.expct;
            let mut qpd = (delta_phase / PI) as i32;
            if qpd >= 0 {
                qpd += qpd & 1;
            } else {
                qpd -= qpd & 1;
            }
            delta_phase -= PI * qpd as f32;

            let true_bin_deviation = self.oversampling as f32 * delta_phase / (2.0 * PI);
            let true_freq = (k as f32 + true_bin_deviation) * self.freq_per_bin;

            self.ana_magn[k] = magn;
            self.ana_freq[k] = true_freq;
        }

        self.syn_magn.fill(0.0);
        self.syn_freq.fill(0.0);

        for k in 0..self.ana_magn.len() {
            let index = ((k as f32) * ratio).round() as usize;
            if index < self.syn_magn.len() {
                self.syn_magn[index] += self.ana_magn[k];
                self.syn_freq[index] = self.ana_freq[k] * ratio;
            }
        }

        self.fft_workspace.fill(Complex32::new(0.0, 0.0));

        for k in 0..self.syn_magn.len() {
            let magn = self.syn_magn[k];
            let mut freq = self.syn_freq[k];
            if freq <= 0.0 {
                freq = k as f32 * self.freq_per_bin;
            }

            let mut phase_diff = (freq / self.freq_per_bin) - k as f32;
            phase_diff = 2.0 * PI * phase_diff / self.oversampling as f32;
            phase_diff += k as f32 * self.expct;

            self.sum_phase[k] += phase_diff;
            let phase = self.sum_phase[k];

            self.fft_workspace[k] = Complex32::new(magn * phase.cos(), magn * phase.sin());
            if k > 0 && k < self.frame_size / 2 {
                let mirrored = self.frame_size - k;
                self.fft_workspace[mirrored] = self.fft_workspace[k].conj();
            }
        }

        self.fft_inverse.process(&mut self.fft_workspace);

        let norm = (self.frame_size_f32 / 2.0) * self.oversampling as f32;
        for k in 0..self.frame_size {
            self.output_accum[k] += self.window[k] * self.fft_workspace[k].re / norm;
        }

        self.out_fifo[..self.step_size].copy_from_slice(&self.output_accum[..self.step_size]);

        self.output_accum.copy_within(self.step_size..(self.step_size + self.frame_size), 0);
        self.output_accum[self.frame_size..].fill(0.0);

        self.in_fifo.copy_within(self.step_size..self.frame_size, 0);
        self.in_fifo[(self.frame_size - self.step_size)..].fill(0.0);
    }
}
