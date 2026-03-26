use crate::dsp::formant::FormantCorrector;
use crate::dsp::psola::{PsolaConfig, PsolaShifter};
use crate::dsp::scale::{midi_to_hz, ScaleKind, ScaleMapper};
use crate::dsp::yin::{PitchEstimate, YinDetector};

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
    pub dead_zone_cents: f32,
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
    shifter: PsolaShifter,
    formant: FormantCorrector,

    confidence_threshold: f32,
    min_freq_hz: f32,
    max_freq_hz: f32,
    sample_rate: f32,
    correction_strength: f32,
    aggressiveness: f32,
    dead_zone_cents: f32,
    dry_level: f32,
    wet_level: f32,
    force_midi_note: Option<u8>,

    // Pitch tracking
    smoothed_pitch_hz: f32,
    pitch_lp_coeff: f32,
    last_reliable_pitch_hz: f32,
    unreliable_blocks: usize,
    max_unreliable_hold_blocks: usize,

    // Ratio: jeden LP filtr sterowany przez retune_time_ms.
    // Poprzednia architektura miała trzy smoothery w szeregu:
    //   pitch LP (10ms) -> OnePoleSmoother (retune_ms) -> limit_ratio_step (slew)
    // Trzy filtry w szeregu = overdamped response. Ratio nigdy nie docierało do celu
    // i oscylowało wokół wartości pośredniej — stąd "jedna nuta" i efekt modulatora.
    current_ratio: f32,
    ratio_lp_coeff: f32,
    retune_time_ms: f32,

    // Analiza pitch: ring buffer + linearyzacja do scratch co hop_size próbek.
    // Poprzedni kod używał analysis_ready=true dopiero po zapełnieniu całego ringu (2048
    // próbek, ~46ms). Przez ten czas PSOLA pracował z ratio=1.0, potem ratio nagle skakało.
    // Teraz ring jest wypełniany inkrementalnie, YIN odpytywany co hop_size próbek
    // synchronicznie z krokiem PSOLA. Brak zimnego startu, brak skoku ratio na początku.
    analysis_ring: Vec<f32>,
    analysis_ring_len: usize,
    analysis_write: usize,
    analysis_scratch: Vec<f32>,
    analysis_hop: usize,
    analysis_hop_counter: usize,
    analysis_filled: usize,

    temp_a: Vec<f32>,
    temp_b: Vec<f32>,
    temp_wet: Vec<f32>,

    dry_delay_line: Vec<f32>,
    dry_delay_idx: usize,

    meter: MeterSnapshot,
    process_max_block: usize,
}

impl PitchCorrectionProcessor {
    pub fn new(config: ProcessorConfig, mapper: ScaleMapper) -> Self {
        let analysis_frame_size = 2048;

        // frame_size=2048 (~46ms @ 44100): wystarczy na pełny okres dla 80Hz (551 próbek).
        // Poprzednie frame_size=1024 było za krótkie — grains nie pokrywały pełnego okresu
        // dla niskich głosów, stąd artefakty fazowe i "metaliczne pudełko".
        // overlap=8 -> step_size=256. Przy frame 2048 OLA jest gładkie nawet przy ratio 1.5+.
        let shifter_cfg = PsolaConfig {
            frame_size: 2048,
            overlap: 8,
        };
        let hop_size = shifter_cfg.frame_size / shifter_cfg.overlap; // 256

        let yin = YinDetector::new(
            config.sample_rate,
            analysis_frame_size,
            config.min_freq_hz,
            config.max_freq_hz,
            config.yin_threshold,
        );

        let shifter = PsolaShifter::new(config.sample_rate, shifter_cfg);
        let dry_latency = shifter.latency_samples();
        let dry_delay_len = (dry_latency + 1).max(1);

        // Pitch LP 8ms: szybkie śledzenie bez jittera między ramkami YIN.
        let pitch_lp_coeff = one_pole_coeff_ms(8.0, config.sample_rate, hop_size);
        // Ratio LP: czas retune definiuje jak szybko PSOLA dochodzi do docelowego ratio.
        let ratio_lp_coeff =
            one_pole_coeff_ms(config.retune_time_ms.max(1.0), config.sample_rate, hop_size);

        let process_max_block = 2048;

        Self {
            yin,
            mapper,
            shifter,
            formant: FormantCorrector::new(config.formant_enabled, config.formant_amount),
            confidence_threshold: config.confidence_threshold.clamp(0.0, 1.0),
            min_freq_hz: config.min_freq_hz,
            max_freq_hz: config.max_freq_hz,
            sample_rate: config.sample_rate,
            correction_strength: config.correction_strength.clamp(0.0, 1.0),
            aggressiveness: config.aggressiveness.clamp(0.0, 1.0),
            dead_zone_cents: config.dead_zone_cents.clamp(0.0, 50.0),
            dry_level: config.dry_level.clamp(0.0, 1.0),
            wet_level: config.wet_level.clamp(0.0, 1.0),
            force_midi_note: config.force_midi_note,
            smoothed_pitch_hz: 0.0,
            pitch_lp_coeff,
            last_reliable_pitch_hz: 0.0,
            unreliable_blocks: 0,
            max_unreliable_hold_blocks: 8,
            current_ratio: 1.0,
            ratio_lp_coeff,
            retune_time_ms: config.retune_time_ms.max(1.0),
            analysis_ring: vec![0.0; analysis_frame_size],
            analysis_ring_len: analysis_frame_size,
            analysis_write: 0,
            analysis_scratch: vec![0.0; analysis_frame_size],
            analysis_hop: hop_size,
            analysis_hop_counter: 0,
            analysis_filled: 0,
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
            process_max_block,
        }
    }

    pub fn set_retune_time_ms(&mut self, time_ms: f32) {
        self.retune_time_ms = time_ms.max(1.0);
        self.ratio_lp_coeff =
            one_pole_coeff_ms(self.retune_time_ms, self.sample_rate, self.analysis_hop);
    }

    pub fn set_correction_strength(&mut self, strength: f32) {
        self.correction_strength = strength.clamp(0.0, 1.0);
    }

    pub fn set_aggressiveness(&mut self, aggressiveness: f32) {
        self.aggressiveness = aggressiveness.clamp(0.0, 1.0);
    }

    pub fn set_dead_zone_cents(&mut self, cents: f32) {
        self.dead_zone_cents = cents.clamp(0.0, 50.0);
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
        for &x in input {
            // Wpisujemy do ring buffera i liczymy ile mamy próbek (do warmup).
            self.analysis_ring[self.analysis_write] = x;
            self.analysis_write = (self.analysis_write + 1) % self.analysis_ring_len;
            if self.analysis_filled < self.analysis_ring_len {
                self.analysis_filled += 1;
            }

            // Co hop_size próbek, gdy mamy pełne okno: linearyzuj ring i odpytaj YIN.
            self.analysis_hop_counter += 1;
            if self.analysis_hop_counter >= self.analysis_hop {
                self.analysis_hop_counter = 0;
                if self.analysis_filled >= self.analysis_ring_len {
                    self.linearize_ring();
                    self.tick_ratio();
                }
            }
        }

        let ratio = self.current_ratio;
        self.meter.ratio = ratio;
        let detected_pitch_hz = self.meter.detected_hz;

        if self.formant.is_enabled() {
            self.formant.preprocess(input, &mut self.temp_a[..input.len()]);
            self.shifter.process_block(
                ratio,
                detected_pitch_hz,
                &self.temp_a[..input.len()],
                &mut self.temp_b[..input.len()],
            );
            self.formant.postprocess(
                &self.temp_b[..input.len()],
                &mut self.temp_wet[..input.len()],
                ratio,
            );
        } else {
            self.shifter.process_block(
                ratio,
                detected_pitch_hz,
                input,
                &mut self.temp_wet[..input.len()],
            );
        }

        let dry_w = self.dry_level.sqrt();
        let wet_w = self.wet_level.sqrt();
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

    /// Kopiuje ring buffer do liniowego scratcha dla YIN.
    /// write pointer wskazuje na najstarszą próbkę — od niej zaczynamy.
    fn linearize_ring(&mut self) {
        let head = self.analysis_write;
        let tail = self.analysis_ring_len - head;
        self.analysis_scratch[..tail].copy_from_slice(&self.analysis_ring[head..]);
        self.analysis_scratch[tail..].copy_from_slice(&self.analysis_ring[..head]);
    }

    /// Wywoływane co hop_size próbek — synchronicznie z krokiem PSOLA.
    fn tick_ratio(&mut self) {
        let estimate = self.yin.estimate(&self.analysis_scratch);
        let tracked_hz = self.update_pitch_tracking(estimate);

        let ratio_target = if tracked_hz <= 0.0 {
            self.meter.target_hz = 0.0;
            1.0_f32
        } else {
            self.compute_target_ratio(tracked_hz)
        };

        // Jeden krok LP — zamiast triple-smoother chain.
        self.current_ratio += (ratio_target - self.current_ratio) * (1.0 - self.ratio_lp_coeff);
        self.current_ratio = self.current_ratio.clamp(0.5, 2.0);
    }

    fn compute_target_ratio(&mut self, tracked_hz: f32) -> f32 {
        let target_hz = if let Some(note) = self.force_midi_note {
            midi_to_hz(note as f32)
        } else {
            self.mapper
                .map_hz_to_scale(tracked_hz)
                .unwrap_or(tracked_hz)
        };

        self.meter.target_hz = target_hz;
        let raw_ratio = (target_hz / tracked_hz).clamp(0.5, 2.0);
        let cents_error = 1200.0 * raw_ratio.log2().abs();

        if cents_error < self.dead_zone_cents {
            return 1.0;
        }

        let ramp_width = 20.0_f32;
        let ramp_factor = ((cents_error - self.dead_zone_cents) / ramp_width).clamp(0.0, 1.0);
        let distance_weight = (cents_error / 80.0).clamp(0.0, 1.0);
        let style_weight = self.aggressiveness + (1.0 - self.aggressiveness) * distance_weight;
        let effective_strength =
            (self.correction_strength * style_weight * ramp_factor).clamp(0.0, 1.0);

        (1.0 + (raw_ratio - 1.0) * effective_strength).clamp(0.5, 2.0)
    }

    fn update_pitch_tracking(&mut self, estimate: Option<PitchEstimate>) -> f32 {
        if let Some(e) = estimate {
            self.meter.confidence = e.confidence;

            let in_range =
                e.frequency_hz >= self.min_freq_hz && e.frequency_hz <= self.max_freq_hz;
            let reliable =
                e.voiced && in_range && e.confidence >= self.confidence_threshold;

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
                    self.smoothed_pitch_hz = self.pitch_lp_coeff * self.smoothed_pitch_hz
                        + (1.0 - self.pitch_lp_coeff) * chosen_pitch;
                }
            } else if self.unreliable_blocks > self.max_unreliable_hold_blocks {
                self.smoothed_pitch_hz = 0.0;
            }

            self.meter.detected_hz = self.smoothed_pitch_hz;
        } else {
            self.meter.detected_hz = 0.0;
            self.meter.confidence = 0.0;
            self.unreliable_blocks = self.unreliable_blocks.saturating_add(1);
            if self.unreliable_blocks > self.max_unreliable_hold_blocks {
                self.smoothed_pitch_hz = 0.0;
                self.last_reliable_pitch_hz = 0.0;
            }
        }

        self.smoothed_pitch_hz
    }
}

fn one_pole_coeff_ms(time_ms: f32, sample_rate: f32, hop_size: usize) -> f32 {
    let clamped_ms = time_ms.max(1.0);
    let tau_samples = (clamped_ms * 0.001 * sample_rate).max(1.0);
    let n = hop_size.max(1) as f32;
    (-n / tau_samples).exp()
}