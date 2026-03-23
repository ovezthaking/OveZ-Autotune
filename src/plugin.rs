use std::num::NonZeroU32;
use std::sync::Arc;

use nih_plug::prelude::*;

use crate::dsp::processor::{PitchCorrectionProcessor, ProcessorConfig};
use crate::dsp::scale::{ScaleKind, ScaleMapper};

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum ScaleParam {
    Chromatic,
    Major,
    Minor,
}

#[derive(Enum, PartialEq, Eq, Clone, Copy, Debug)]
pub enum KeyParam {
    C,
    Cs,
    D,
    Ds,
    E,
    F,
    Fs,
    G,
    Gs,
    A,
    As,
    B,
}

impl From<ScaleParam> for ScaleKind {
    fn from(value: ScaleParam) -> Self {
        match value {
            ScaleParam::Chromatic => ScaleKind::Chromatic,
            ScaleParam::Major => ScaleKind::Major,
            ScaleParam::Minor => ScaleKind::Minor,
        }
    }
}

impl KeyParam {
    fn pitch_class(self) -> i32 {
        match self {
            KeyParam::C => 0,
            KeyParam::Cs => 1,
            KeyParam::D => 2,
            KeyParam::Ds => 3,
            KeyParam::E => 4,
            KeyParam::F => 5,
            KeyParam::Fs => 6,
            KeyParam::G => 7,
            KeyParam::Gs => 8,
            KeyParam::A => 9,
            KeyParam::As => 10,
            KeyParam::B => 11,
        }
    }
}

#[derive(Params)]
pub struct RustAutotuneParams {
    #[id = "dry"]
    pub dry: FloatParam,

    #[id = "wet"]
    pub wet: FloatParam,

    #[id = "bypass"]
    pub bypass: BoolParam,

    #[id = "retune_ms"]
    pub retune_ms: FloatParam,

    #[id = "strength"]
    pub strength: FloatParam,

    #[id = "aggr"]
    pub aggressiveness: FloatParam,

    #[id = "scale"]
    pub scale: EnumParam<ScaleParam>,

    #[id = "key"]
    pub key: EnumParam<KeyParam>,
}

impl Default for RustAutotuneParams {
    fn default() -> Self {
        Self {
            dry: FloatParam::new("Dry", 0.0, FloatRange::Linear { min: 0.0, max: 100.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_unit(" %"),
            wet: FloatParam::new("Wet", 100.0, FloatRange::Linear { min: 0.0, max: 100.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_unit(" %"),
            bypass: BoolParam::new("Bypass", false),
            retune_ms: FloatParam::new(
                "Retune Speed",
                35.0,
                FloatRange::Skewed {
                    min: 1.0,
                    max: 200.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            .with_unit(" ms"),
            strength: FloatParam::new("Strength", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(""),
            aggressiveness: FloatParam::new(
                "Aggressiveness",
                0.8,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
                .with_unit(""),
            scale: EnumParam::new("Scale", ScaleParam::Chromatic),
            key: EnumParam::new("Key", KeyParam::C),
        }
    }
}

pub struct RustAutotunePlugin {
    params: Arc<RustAutotuneParams>,
    processor: Option<PitchCorrectionProcessor>,
    input_mono: Vec<f32>,
    output_mono: Vec<f32>,
}

impl Default for RustAutotunePlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(RustAutotuneParams::default()),
            processor: None,
            input_mono: Vec::new(),
            output_mono: Vec::new(),
        }
    }
}

impl Plugin for RustAutotunePlugin {
    const NAME: &'static str = "Rust AutoTune";
    const VENDOR: &'static str = "Rust DSP";
    const URL: &'static str = "https://example.com";
    const EMAIL: &'static str = "dev@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames::const_default(),
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames::const_default(),
        },
    ];

    const HARD_REALTIME_ONLY: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        let max_buffer = buffer_config.max_buffer_size as usize;
        self.input_mono = vec![0.0; max_buffer.max(64)];
        self.output_mono = vec![0.0; max_buffer.max(64)];

        let mapper = ScaleMapper::new(0, ScaleKind::Chromatic);
        let proc_cfg = ProcessorConfig {
            sample_rate: buffer_config.sample_rate,
            min_freq_hz: 80.0,
            max_freq_hz: 1000.0,
            yin_threshold: 0.12,
            confidence_threshold: 0.75,
            retune_time_ms: 35.0,
            correction_strength: 1.0,
            aggressiveness: 0.8,
            dry_level: 0.0,
            wet_level: 1.0,
            force_midi_note: None,
            formant_enabled: true,
            formant_amount: 0.55,
        };

        self.processor = Some(PitchCorrectionProcessor::new(proc_cfg, mapper));
        true
    }

    fn reset(&mut self) {}

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        if self.params.bypass.value() {
            return ProcessStatus::Normal;
        }

        let Some(processor) = self.processor.as_mut() else {
            return ProcessStatus::Normal;
        };

        let retune_ms = self.params.retune_ms.value();
        let strength = self.params.strength.value();
        let aggressiveness = self.params.aggressiveness.value();
        let scale = self.params.scale.value();
        let key = self.params.key.value();

        processor.set_retune_time_ms(retune_ms);
        processor.set_correction_strength(strength);
        processor.set_aggressiveness(aggressiveness);
        processor.set_scale_key(scale.into(), key.pitch_class());
        processor.set_dry_wet_levels(0.0, 1.0);

        let samples = buffer.samples();
        if samples == 0 {
            return ProcessStatus::Normal;
        }

        if samples > self.input_mono.len() {
            return ProcessStatus::Normal;
        }

        for (i, channel_samples) in buffer.iter_samples().enumerate() {
            let mut sum = 0.0;
            let mut count = 0usize;
            for sample in channel_samples {
                sum += *sample;
                count += 1;
            }
            self.input_mono[i] = if count > 0 { sum / count as f32 } else { 0.0 };
        }

        processor.process_block(&self.input_mono[..samples], &mut self.output_mono[..samples]);

        for (i, channel_samples) in buffer.iter_samples().enumerate() {
            let dry = (self.params.dry.smoothed.next() / 100.0).clamp(0.0, 1.0);
            let wet = (self.params.wet.smoothed.next() / 100.0).clamp(0.0, 1.0);
            let tuned = self.output_mono[i];

            for sample in channel_samples {
                *sample = (*sample * dry + tuned * wet).clamp(-1.0, 1.0);
            }
        }

        ProcessStatus::Normal
    }
}

impl ClapPlugin for RustAutotunePlugin {
    const CLAP_ID: &'static str = "com.rustdsp.rust_autotune";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Real-time pitch correction plugin");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::PitchShifter,
    ];
}

impl Vst3Plugin for RustAutotunePlugin {
    const VST3_CLASS_ID: [u8; 16] = *b"RustAutoTuneDSP!";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::PitchShift];
}

nih_export_clap!(RustAutotunePlugin);
nih_export_vst3!(RustAutotunePlugin);
