#[derive(Debug, Clone)]
pub struct FormantCorrector {
    enabled: bool,
    amount: f32,
    pre_emphasis: f32,
    pre_state: f32,
    de_state: f32,
}

impl FormantCorrector {
    pub fn new(enabled: bool, amount: f32) -> Self {
        Self {
            enabled,
            amount: amount.clamp(0.0, 1.0),
            pre_emphasis: 0.95,
            pre_state: 0.0,
            de_state: 0.0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && self.amount > 0.0
    }

    pub fn preprocess(&mut self, input: &[f32], out: &mut [f32]) {
        if !self.is_enabled() {
            out.copy_from_slice(input);
            return;
        }

        for (x, y) in input.iter().copied().zip(out.iter_mut()) {
            let hp = x - self.pre_emphasis * self.pre_state;
            self.pre_state = x;
            *y = hp;
        }
    }

    pub fn postprocess(&mut self, shifted: &[f32], out: &mut [f32], pitch_ratio: f32) {
        if !self.is_enabled() {
            out.copy_from_slice(shifted);
            return;
        }

        let correction = (1.0 / pitch_ratio.max(0.5)).powf(self.amount).clamp(0.5, 2.0);
        let deemph = (self.pre_emphasis * correction).clamp(0.0, 0.99);

        for (x, y) in shifted.iter().copied().zip(out.iter_mut()) {
            let v = x + deemph * self.de_state;
            self.de_state = v;
            *y = v;
        }
    }
}
