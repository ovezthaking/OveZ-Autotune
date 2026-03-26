use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, SampleRate, Stream, StreamConfig};
use rtrb::RingBuffer;

use crate::config::RuntimeConfig;
use crate::dsp::processor::{PitchCorrectionProcessor, ProcessorConfig};
use crate::dsp::scale::{parse_root_note, ScaleMapper};

const RING_CAPACITY: usize = 16384;

pub fn run_realtime(config: RuntimeConfig) -> Result<()> {
    let host = cpal::default_host();

    let input_device = host
        .default_input_device()
        .context("Brak domyslnego urzadzenia wejsciowego")?;
    let output_device = host
        .default_output_device()
        .context("Brak domyslnego urzadzenia wyjsciowego")?;

    let input_default = input_device.default_input_config()?;
    let output_default = output_device.default_output_config()?;

    if input_default.sample_format() != SampleFormat::F32 {
        return Err(anyhow!(
            "Wejscie audio musi wspierac f32. Aktualnie: {:?}",
            input_default.sample_format()
        ));
    }
    if output_default.sample_format() != SampleFormat::F32 {
        return Err(anyhow!(
            "Wyjscie audio musi wspierac f32. Aktualnie: {:?}",
            output_default.sample_format()
        ));
    }

    let sample_rate = config
        .sample_rate
        .unwrap_or(output_default.sample_rate().0)
        .max(32000)
        .min(96000);

    let input_channels = input_default.channels() as usize;
    let output_channels = output_default.channels() as usize;

    let stream_config_in = StreamConfig {
        channels: input_default.channels(),
        sample_rate: SampleRate(sample_rate),
        buffer_size: BufferSize::Fixed(config.block_size),
    };

    let stream_config_out = StreamConfig {
        channels: output_default.channels(),
        sample_rate: SampleRate(sample_rate),
        buffer_size: BufferSize::Fixed(config.block_size),
    };

    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);

    let underruns = Arc::new(AtomicU64::new(0));
    let overruns = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    let root_pc = parse_root_note(&config.root_note)
        .ok_or_else(|| anyhow!("Nieprawidlowa nuta root: {}", config.root_note))?;
    let mapper = ScaleMapper::new(root_pc, config.scale.into());

    let proc_cfg = ProcessorConfig {
        sample_rate: sample_rate as f32,
        min_freq_hz: config.min_freq_hz,
        max_freq_hz: config.max_freq_hz,
        yin_threshold: config.yin_threshold,
        confidence_threshold: config.confidence_threshold,
        retune_time_ms: config.retune_time_ms,
        correction_strength: config.correction_strength,
        aggressiveness: config.aggressiveness,
        dry_level: config.dry_level,
        wet_level: config.wet_level,
        force_midi_note: config.force_midi_note,
        formant_enabled: config.formant_enabled,
        formant_amount: config.formant_amount,
        dead_zone_cents: config.dead_zone_cents,
    };

    let underruns_out = underruns.clone();
    let input_stream = build_input_stream(
        &input_device,
        &stream_config_in,
        input_channels,
        producer,
        overruns.clone(),
    )?;

    let output_stream = build_output_stream(
        &output_device,
        &stream_config_out,
        output_channels,
        consumer,
        underruns_out,
        proc_cfg,
        mapper,
    )?;

    let running_signal = running.clone();
    ctrlc::set_handler(move || {
        running_signal.store(false, Ordering::Relaxed);
    })
    .context("Nie udalo sie ustawic obslugi Ctrl+C")?;

    println!(
        "Start AutoTune CLI | sample_rate={} Hz | block_size={} | channels_in={} | channels_out={}",
        sample_rate, config.block_size, input_channels, output_channels
    );
    println!("Nacisnij Ctrl+C aby zakonczyc.");

    input_stream.play()?;
    output_stream.play()?;

    while running.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        let u = underruns.load(Ordering::Relaxed);
        let o = overruns.load(Ordering::Relaxed);
        if u > 0 || o > 0 {
            println!("Audio stats | underruns={} overruns={}", u, o);
        }
    }

    Ok(())
}

fn build_input_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    mut producer: rtrb::Producer<f32>,
    overruns: Arc<AtomicU64>,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("Input stream error: {err}");

    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _| {
            for frame in data.chunks(channels) {
                let mono = frame.iter().copied().sum::<f32>() / channels as f32;
                if producer.push(mono).is_err() {
                    overruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}

fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    mut consumer: rtrb::Consumer<f32>,
    underruns: Arc<AtomicU64>,
    proc_cfg: ProcessorConfig,
    mapper: ScaleMapper,
) -> Result<Stream> {
    let mut processor = PitchCorrectionProcessor::new(proc_cfg, mapper);

    let max_frames = 2048;
    let mut input_block = vec![0.0f32; max_frames];
    let mut output_block = vec![0.0f32; max_frames];

    let err_fn = |err| eprintln!("Output stream error: {err}");

    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _| {
            let frames = data.len() / channels;
            if frames > input_block.len() {
                return;
            }

            for x in &mut input_block[..frames] {
                *x = match consumer.pop() {
                    Ok(v) => v,
                    Err(_) => {
                        underruns.fetch_add(1, Ordering::Relaxed);
                        0.0
                    }
                };
            }

            processor.process_block(&input_block[..frames], &mut output_block[..frames]);
            let meter = processor.meter();
            let _ = meter;

            for (frame, y) in data
                .chunks_mut(channels)
                .zip(output_block[..frames].iter().copied())
            {
                for sample in frame {
                    *sample = y;
                }
            }
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}
