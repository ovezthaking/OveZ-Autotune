#[derive(Debug, Clone, Copy)]
pub struct PitchEstimate {
    pub frequency_hz: f32,
    pub confidence: f32,
    pub voiced: bool,
}

#[derive(Debug, Clone)]
pub struct YinDetector {
    sample_rate: f32,
    min_tau: usize,
    max_tau: usize,
    threshold: f32,
    energy_floor: f32,
    window: Vec<f32>,
    work: Vec<f32>,
    diff: Vec<f32>,
    cmndf: Vec<f32>,
}

impl YinDetector {
    pub fn new(
        sample_rate: f32,
        frame_size: usize,
        min_freq_hz: f32,
        max_freq_hz: f32,
        threshold: f32,
    ) -> Self {
        let max_tau = (sample_rate / min_freq_hz.max(20.0)).round() as usize;
        let min_tau = (sample_rate / max_freq_hz.max(min_freq_hz + 1.0)).round() as usize;
        let tau_cap = max_tau.max(min_tau + 2).min(frame_size.saturating_sub(2));

        let mut window = vec![0.0; frame_size];
        for (i, w) in window.iter_mut().enumerate() {
            *w = 0.5
                - 0.5
                    * (2.0 * std::f32::consts::PI * i as f32 / frame_size.max(1) as f32).cos();
        }

        Self {
            sample_rate,
            min_tau,
            max_tau: tau_cap,
            threshold: threshold.clamp(0.01, 0.40),
            energy_floor: 1.0e-5,
            window,
            work: vec![0.0; frame_size],
            diff: vec![0.0; tau_cap + 2],
            cmndf: vec![1.0; tau_cap + 2],
        }
    }

    pub fn estimate(&mut self, frame: &[f32]) -> Option<PitchEstimate> {
        if frame.len() < self.max_tau + 2 || frame.len() > self.work.len() {
            return None;
        }

        // Usuwamy DC i stosujemy Hann, aby ograniczyc edge effects i fałszywe minima CMNDF.
        let mean = frame.iter().copied().sum::<f32>() / frame.len().max(1) as f32;
        for i in 0..frame.len() {
            self.work[i] = (frame[i] - mean) * self.window[i];
        }

        let rms =
            (self.work[..frame.len()].iter().map(|x| x * x).sum::<f32>() / frame.len() as f32)
                .sqrt();
        if rms < self.energy_floor {
            return Some(PitchEstimate {
                frequency_hz: 0.0,
                confidence: 0.0,
                voiced: false,
            });
        }

        let n = frame.len();
        for tau in self.min_tau..=self.max_tau {
            let mut acc = 0.0;
            for i in 0..(n - tau) {
                let d = self.work[i] - self.work[i + tau];
                acc += d * d;
            }
            self.diff[tau] = acc;
        }

        self.cmndf[0] = 1.0;
        let mut running_sum = 0.0;
        for tau in self.min_tau..=self.max_tau {
            running_sum += self.diff[tau];
            self.cmndf[tau] = if running_sum > 0.0 {
                self.diff[tau] * (tau as f32) / running_sum
            } else {
                1.0
            };
        }

        let mut tau_est = None;
        for tau in self.min_tau..=self.max_tau {
            if self.cmndf[tau] < self.threshold {
                let mut t = tau;
                while t + 1 <= self.max_tau && self.cmndf[t + 1] < self.cmndf[t] {
                    t += 1;
                }
                tau_est = Some(t);
                break;
            }
        }

        if tau_est.is_none() {
            let mut best_tau = self.min_tau;
            let mut best_val = self.cmndf[self.min_tau];
            for tau in (self.min_tau + 1)..=self.max_tau {
                if self.cmndf[tau] < best_val {
                    best_val = self.cmndf[tau];
                    best_tau = tau;
                }
            }
            tau_est = Some(best_tau);
        }

        let tau = tau_est?;
        if tau <= self.min_tau || tau >= self.max_tau {
            return None;
        }

        let x0 = self.cmndf[tau - 1];
        let x1 = self.cmndf[tau];
        let x2 = self.cmndf[tau + 1];
        let denom = 2.0 * (2.0 * x1 - x0 - x2);
        let delta = if denom.abs() > 1.0e-9 {
            (x0 - x2) / denom
        } else {
            0.0
        }
        .clamp(-1.0, 1.0);

        let tau_refined = (tau as f32 + delta).max(1.0);
        let frequency_hz = self.sample_rate / tau_refined;
        let confidence = (1.0 - self.cmndf[tau]).clamp(0.0, 1.0);

        Some(PitchEstimate {
            frequency_hz,
            confidence,
            voiced: confidence >= (1.0 - self.threshold),
        })
    }
}
