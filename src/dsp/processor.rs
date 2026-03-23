use crate::dsp::formant::FormantCorrector;
use crate::dsp::psola::{PsolaConfig, PsolaShifter};
use crate::dsp::scale::{midi_to_hz, ScaleKind, ScaleMapper};
use crate::dsp::smoothing::OnePoleSmoother;
use crate::dsp::yin::{PitchEstimate, YinDetector};
use dasp::interpolate::linear::Linear;
use dasp::interpolate::Interpolator;

#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    pub sample_rate: f32,
    pub min_freq_hz: f32,
    pub max_freq_hz: f32,
    pub yin_threshold: f32,
    pub confidence_threshold: f32,
    pub retune_time_ms: f32,
    pub correction_strength: f32,
    pub aggressiveness: f32,
    pub dry_level: f32,
    pub wet_level: f32,
    pub force_midi_note: Option<u8>,
    pub formant_enabled: bool,
    pub formant_amount: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct MeterSnapshot {
    pub detected_hz: f32,
    pub confidence: f32,
    pub target_hz: f32,
    pub ratio: f32,
}

pub struct PitchCorrectionProcessor {
    yin: YinDetector,
    mapper: ScaleMapper,
    smoother: OnePoleSmoother,
    shifter: PsolaShifter,
    formant: FormantCorrector,

    confidence_threshold: f32,
    min_freq_hz: f32,
    max_freq_hz: f32,
    sample_rate: f32,
    hop_size: usize,
    retune_time_ms: f32,
    pitch_smoothing_coeff: f32,
    smoothed_pitch_hz: f32,
    last_reliable_pitch_hz: f32,
    unreliable_blocks: usize,
    max_unreliable_hold_blocks: usize,
    correction_strength: f32,
    aggressiveness: f32,
    dry_level: f32,
    wet_level: f32,
    force_midi_note: Option<u8>,

    analysis_ring: Vec<f32>,
    analysis_idx: usize,
    analysis_scratch: Vec<f32>,
    analysis_ready: bool,

    temp_a: Vec<f32>,
    temp_b: Vec<f32>,
    temp_wet: Vec<f32>,

    dry_delay_line: Vec<f32>,
    dry_delay_idx: usize,

    meter: MeterSnapshot,
    last_ratio: f32,
    ratio_slew_cents_per_sec: f32,
    process_max_block: usize,
}

impl PitchCorrectionProcessor {
    pub fn new(config: ProcessorConfig, mapper: ScaleMapper) -> Self {
        let analysis_frame_size = 2048;
        let shifter_cfg = PsolaConfig {
            frame_size: 1024,
            overlap: 4,
        };

        let yin = YinDetector::new(
            config.sample_rate,
            analysis_frame_size,
            config.min_freq_hz,
            config.max_freq_hz,
            config.yin_threshold,
        );

        let smoother = OnePoleSmoother::new(
            1.0,
            config.retune_time_ms,
            config.sample_rate,
            shifter_cfg.frame_size / shifter_cfg.overlap,
        );

        let process_max_block = 2048;
        let hop_size = shifter_cfg.frame_size / shifter_cfg.overlap;
        let pitch_smoothing_coeff = one_pole_coeff_ms(35.0, config.sample_rate, hop_size);
        let shifter = PsolaShifter::new(config.sample_rate, shifter_cfg);
        let dry_latency = shifter.latency_samples();
        let dry_delay_len = (dry_latency + 1).max(1);

        Self {
            yin,
            mapper,
            smoother,
            shifter,
            formant: FormantCorrector::new(config.formant_enabled, config.formant_amount),
            confidence_threshold: config.confidence_threshold.clamp(0.0, 1.0),
            min_freq_hz: config.min_freq_hz,
            max_freq_hz: config.max_freq_hz,
            sample_rate: config.sample_rate,
            hop_size,
            retune_time_ms: config.retune_time_ms.max(1.0),
            pitch_smoothing_coeff,
            smoothed_pitch_hz: 0.0,
            last_reliable_pitch_hz: 0.0,
            unreliable_blocks: 0,
            max_unreliable_hold_blocks: 14,
            correction_strength: config.correction_strength.clamp(0.0, 1.0),
            aggressiveness: config.aggressiveness.clamp(0.0, 1.0),
            dry_level: config.dry_level.clamp(0.0, 1.0),
            wet_level: config.wet_level.clamp(0.0, 1.0),
            force_midi_note: config.force_midi_note,
            analysis_ring: vec![0.0; analysis_frame_size],
            analysis_idx: 0,
            analysis_scratch: vec![0.0; analysis_frame_size],
            analysis_ready: false,
            temp_a: vec![0.0; process_max_block],
            temp_b: vec![0.0; process_max_block],
            temp_wet: vec![0.0; process_max_block],
            dry_delay_line: vec![0.0; dry_delay_len],
            dry_delay_idx: 0,
            meter: MeterSnapshot {
                detected_hz: 0.0,
                confidence: 0.0,
                target_hz: 0.0,
                ratio: 1.0,
            },
            last_ratio: 1.0,
            ratio_slew_cents_per_sec: 2800.0,
            process_max_block,
        }
    }

    pub fn set_retune_time_ms(&mut self, time_ms: f32) {
        self.retune_time_ms = time_ms.max(1.0);
        self.smoother
            .set_time_ms(self.retune_time_ms, self.sample_rate, self.hop_size);
    }

    pub fn set_correction_strength(&mut self, strength: f32) {
        self.correction_strength = strength.clamp(0.0, 1.0);
    }

    pub fn set_aggressiveness(&mut self, aggressiveness: f32) {
        self.aggressiveness = aggressiveness.clamp(0.0, 1.0);
    }

    pub fn set_scale_key(&mut self, kind: ScaleKind, root_pc: i32) {
        self.mapper.set_scale(root_pc, kind);
    }

    pub fn set_dry_wet_levels(&mut self, dry_level: f32, wet_level: f32) {
        self.dry_level = dry_level.clamp(0.0, 1.0);
        self.wet_level = wet_level.clamp(0.0, 1.0);
    }

    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());

        let mut offset = 0;
        while offset < input.len() {
            let n = (input.len() - offset).min(self.process_max_block);
            self.process_chunk(&input[offset..offset + n], &mut output[offset..offset + n]);
            offset += n;
        }
    }

    pub fn meter(&self) -> MeterSnapshot {
        self.meter
    }

    fn process_chunk(&mut self, input: &[f32], output: &mut [f32]) {
        self.push_analysis(input);

        let ratio_target = self.target_ratio_from_detection();
        let ratio_smoothed = self.smoother.process(ratio_target).clamp(0.5, 2.0);
        let ratio_limited = self.limit_ratio_step(ratio_smoothed, input.len()).clamp(0.5, 2.0);

        // Dodatkowe gladzenie w ramach bloku redukuje skokowe zmiany pitch ratio.
        let ratio_for_block = if input.len() > 1 {
            let interp = Linear::new([self.last_ratio], [ratio_limited]);
            let mut acc = 0.0;
            for i in 0..input.len() {
                let x = i as f64 / (input.len() - 1) as f64;
                acc += interp.interpolate(x)[0];
            }
            acc / input.len() as f32
        } else {
            ratio_limited
        }
        .clamp(0.5, 2.0);

        self.last_ratio = ratio_limited;
        self.meter.ratio = ratio_for_block;
        let detected_pitch_hz = self.meter.detected_hz;

        if self.formant.is_enabled() {
            self.formant.preprocess(input, &mut self.temp_a[..input.len()]);
            self.shifter.process_block(
                ratio_for_block,
                detected_pitch_hz,
                &self.temp_a[..input.len()],
                &mut self.temp_b[..input.len()],
            );
            self.formant.postprocess(
                &self.temp_b[..input.len()],
                &mut self.temp_wet[..input.len()],
                ratio_for_block,
            );
        } else {
            self.shifter.process_block(
                ratio_for_block,
                detected_pitch_hz,
                input,
                &mut self.temp_wet[..input.len()],
            );
        }

        let dry = self.dry_level;
        let wet = self.wet_level;

        // Equal-power blend ogranicza percepcyjne skoki glosnosci przy zmianie proporcji.
        // Dodatkowo normalizacja po sumie wag stabilizuje poziom dla niezaleznych dry/wet.
        let dry_w = dry.sqrt();
        let wet_w = wet.sqrt();
        let norm = (dry_w + wet_w).max(1.0);
        let delay_len = self.dry_delay_line.len();

        for i in 0..input.len() {
            let dry_sample = self.dry_delay_line[self.dry_delay_idx];
            self.dry_delay_line[self.dry_delay_idx] = input[i];
            self.dry_delay_idx += 1;
            if self.dry_delay_idx >= delay_len {
                self.dry_delay_idx = 0;
            }

            let y = (dry_w * dry_sample + wet_w * self.temp_wet[i]) / norm;
            output[i] = y.clamp(-1.0, 1.0);
        }
    }

    fn push_analysis(&mut self, input: &[f32]) {
        for x in input {
            self.analysis_ring[self.analysis_idx] = *x;
            self.analysis_idx += 1;
            if self.analysis_idx >= self.analysis_ring.len() {
                self.analysis_idx = 0;
                self.analysis_ready = true;
            }
        }
    }

    fn target_ratio_from_detection(&mut self) -> f32 {
        if !self.analysis_ready {
            return 1.0;
        }

        self.copy_latest_frame();
        let estimate = self.yin.estimate(&self.analysis_scratch);
        let tracked_pitch_hz = self.update_pitch_tracking(estimate);

        if tracked_pitch_hz <= 0.0 {
            self.meter.target_hz = 0.0;
            return 1.0;
        }

        let target_hz = if let Some(note) = self.force_midi_note {
            midi_to_hz(note as f32)
        } else {
            self.mapper
                .map_hz_to_scale(tracked_pitch_hz)
                .unwrap_or(tracked_pitch_hz)
        };

        self.meter.target_hz = target_hz;
        let raw_ratio = (target_hz / tracked_pitch_hz).clamp(0.5, 2.0);

        // Korekcja jest 2-etapowa:
        // 1) SNAP: target_hz to najblizsza nuta z wybranej skali.
        // 2) STRENGTH/AGGRESSIVENESS: kontrolujemy jak mocno i jak "twardo"
        //    glos ma byc dociagany do tej nuty.
        //
        // Dla niskiej aggressiveness korekcja jest lagodna przy malych odstrojeniach,
        // a dla wysokiej aggressiveness utrzymuje silny snap nawet blisko celu,
        // dajac klasyczny hard-tune character.
        let cents_error = 1200.0 * raw_ratio.log2().abs();
        let distance_weight = (cents_error / 80.0).clamp(0.0, 1.0);
        let style_weight =
            self.aggressiveness + (1.0 - self.aggressiveness) * distance_weight;
        let effective_strength = (self.correction_strength * style_weight).clamp(0.0, 1.0);

        (1.0 + (raw_ratio - 1.0) * effective_strength).clamp(0.5, 2.0)
    }

    fn copy_latest_frame(&mut self) {
        let n = self.analysis_ring.len();
        let head = self.analysis_idx;
        let first = n - head;
        self.analysis_scratch[..first].copy_from_slice(&self.analysis_ring[head..]);
        self.analysis_scratch[first..].copy_from_slice(&self.analysis_ring[..head]);
    }

    fn update_pitch_tracking(&mut self, estimate: Option<PitchEstimate>) -> f32 {
        if let Some(e) = estimate {
            self.meter.confidence = e.confidence;

            let in_range = e.frequency_hz >= self.min_freq_hz && e.frequency_hz <= self.max_freq_hz;
            let reliable = e.voiced && in_range && e.confidence >= self.confidence_threshold;

            // Confidence gate: nie pozwalamy, by niepewne ramki nadpisywaly tor F0,
            // bo to powoduje jitter pitch-ratio i slyszalne artefakty modulacyjne.
            let chosen_pitch = if reliable {
                self.unreliable_blocks = 0;
                self.last_reliable_pitch_hz = e.frequency_hz;
                e.frequency_hz
            } else {
                self.unreliable_blocks = self.unreliable_blocks.saturating_add(1);
                if self.unreliable_blocks <= self.max_unreliable_hold_blocks {
                    self.last_reliable_pitch_hz
                } else {
                    0.0
                }
            };

            if chosen_pitch > 0.0 {
                if self.smoothed_pitch_hz <= 0.0 {
                    self.smoothed_pitch_hz = chosen_pitch;
                } else {
                    // Exponential smoothing (one-pole LP) stabilizuje F0 miedzy ramkami.
                    // Bez tego drobny jitter detektora przeklada sie na ciagla modulacje
                    // pitch-shift ratio, co slychac jako "buzz", "warble" i "boxy".
                    self.smoothed_pitch_hz = self.pitch_smoothing_coeff * self.smoothed_pitch_hz
                        + (1.0 - self.pitch_smoothing_coeff) * chosen_pitch;
                }
            }

            self.meter.detected_hz = self.smoothed_pitch_hz;
        } else {
            self.meter.detected_hz = 0.0;
            self.meter.confidence = 0.0;
            self.unreliable_blocks = self.unreliable_blocks.saturating_add(1);
            if self.unreliable_blocks <= self.max_unreliable_hold_blocks {
                self.smoothed_pitch_hz = self.last_reliable_pitch_hz;
            } else {
                self.smoothed_pitch_hz = 0.0;
                self.last_reliable_pitch_hz = 0.0;
            }
        }

        self.smoothed_pitch_hz
    }

    fn limit_ratio_step(&self, ratio_target: f32, block_len: usize) -> f32 {
        let prev = self.last_ratio.max(1.0e-6);
        let dt = block_len as f32 / self.sample_rate.max(1.0);
        let max_cents = (self.ratio_slew_cents_per_sec * dt).max(1.0);
        let up = 2.0_f32.powf(max_cents / 1200.0);
        let down = 1.0 / up;
        ratio_target.clamp(prev * down, prev * up)
    }
}

fn one_pole_coeff_ms(time_ms: f32, sample_rate: f32, hop_size: usize) -> f32 {
    let clamped_ms = time_ms.max(1.0);
    let tau_samples = (clamped_ms * 0.001 * sample_rate).max(1.0);
    let n = hop_size.max(1) as f32;
    (-n / tau_samples).exp()
}
