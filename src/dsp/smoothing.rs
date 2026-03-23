#[derive(Debug, Clone)]
pub struct OnePoleSmoother {
    current: f32,
    coeff: f32,
}

impl OnePoleSmoother {
    pub fn new(initial: f32, time_ms: f32, sample_rate: f32, hop_size: usize) -> Self {
        let coeff = coeff_from_time_ms(time_ms, sample_rate, hop_size);
        Self {
            current: initial,
            coeff,
        }
    }

    pub fn set_time_ms(&mut self, time_ms: f32, sample_rate: f32, hop_size: usize) {
        self.coeff = coeff_from_time_ms(time_ms, sample_rate, hop_size);
    }

    pub fn process(&mut self, target: f32) -> f32 {
        self.current += (target - self.current) * (1.0 - self.coeff);
        self.current
    }
}

fn coeff_from_time_ms(time_ms: f32, sample_rate: f32, hop_size: usize) -> f32 {
    let clamped_ms = time_ms.max(1.0);
    let tau_samples = (clamped_ms * 0.001 * sample_rate).max(1.0);
    let n = hop_size.max(1) as f32;
    (-n / tau_samples).exp()
}
