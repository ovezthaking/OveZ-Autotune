use anyhow::Result;
use clap::Parser;

use rust_autotune::config::{RuntimeConfig, ScaleArg};

#[derive(Parser, Debug)]
#[command(author, version, about = "Real-time Auto-Tune-like pitch correction in Rust")]
struct Cli {
    #[arg(long, default_value_t = 256)]
    block_size: u32,

    #[arg(long)]
    sample_rate: Option<u32>,

    #[arg(long, default_value_t = 80.0)]
    min_freq: f32,

    #[arg(long, default_value_t = 1000.0)]
    max_freq: f32,

    #[arg(long, default_value_t = 0.12)]
    yin_threshold: f32,

    #[arg(long, default_value_t = 0.75)]
    confidence_threshold: f32,

    #[arg(long, default_value_t = 40.0)]
    retune_ms: f32,

    #[arg(long, default_value_t = 0.0)]
    strength: f32,

    #[arg(long, default_value_t = 0.7)]
    aggressiveness: f32,

    #[arg(long, value_enum, default_value_t = ScaleArg::Chromatic)]
    scale: ScaleArg,

    #[arg(long, default_value = "C")]
    root: String,

    #[arg(long, default_value_t = 100.0)]
    wet: f32,

    #[arg(long, default_value_t = 0.0)]
    dry: f32,

    #[arg(long, default_value_t = false)]
    formant: bool,

    #[arg(long, default_value_t = 0.6)]
    formant_amount: f32,

    #[arg(long)]
    midi_note: Option<u8>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg = RuntimeConfig {
        block_size: cli.block_size.clamp(128, 512),
        sample_rate: cli.sample_rate,
        min_freq_hz: cli.min_freq.max(40.0),
        max_freq_hz: cli.max_freq.min(2000.0).max(cli.min_freq + 20.0),
        yin_threshold: cli.yin_threshold,
        confidence_threshold: cli.confidence_threshold,
        retune_time_ms: cli.retune_ms,
        correction_strength: cli.strength.clamp(0.0, 1.0),
        aggressiveness: cli.aggressiveness.clamp(0.0, 1.0),
        scale: cli.scale,
        root_note: cli.root,
        dry_level: (cli.dry / 100.0).clamp(0.0, 1.0),
        wet_level: (cli.wet / 100.0).clamp(0.0, 1.0),
        formant_enabled: cli.formant,
        formant_amount: cli.formant_amount.clamp(0.0, 1.0),
        force_midi_note: cli.midi_note,
    };

    rust_autotune::audio::run_realtime(cfg)
}
