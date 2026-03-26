use clap::ValueEnum;

use crate::dsp::scale::ScaleKind;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScaleArg {
    Chromatic,
    Major,
    Minor,
}

impl From<ScaleArg> for ScaleKind {
    fn from(value: ScaleArg) -> Self {
        match value {
            ScaleArg::Chromatic => ScaleKind::Chromatic,
            ScaleArg::Major => ScaleKind::Major,
            ScaleArg::Minor => ScaleKind::Minor,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub block_size: u32,
    pub sample_rate: Option<u32>,
    pub min_freq_hz: f32,
    pub max_freq_hz: f32,
    pub yin_threshold: f32,
    pub confidence_threshold: f32,
    pub retune_time_ms: f32,
    pub correction_strength: f32,
    pub aggressiveness: f32,
    /// Martwa strefa w centach — odchylenia mniejsze niż ta wartość nie są korygowane.
    /// 0 = hard-tune (zawsze koryguj), 20-30 = naturalne brzmienie.
    pub dead_zone_cents: f32,
    pub scale: ScaleArg,
    pub root_note: String,
    pub dry_level: f32,
    pub wet_level: f32,
    pub formant_enabled: bool,
    pub formant_amount: f32,
    pub force_midi_note: Option<u8>,
}
