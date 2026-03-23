# Rust Autotune (Real-Time CLI)

Profesjonalny, niskolatencyjny pitch-correction w Rust, z naciskiem na jakosc i bezpieczenstwo real-time.

## Cechy

- Audio I/O przez `cpal` (wejscie i wyjscie na zywo)
- YIN pitch detection z progiem pewnosci i bramka energii
- Mapowanie do skali: chromatic / major / minor + wybieralny root
- Regulowany retune speed (smoothing ratio)
- Pitch shifting przez TD-PSOLA (Pitch-Synchronous Overlap and Add)
- Dry/Wet mix
- Podstawowa korekcja formantow (pre/de-emphasis)
- Lock-free bufor miedzy callbackami audio (`rtrb`)

## Architektura

- `src/audio`:
  - Integracja `cpal`
  - Lock-free transfer sygnalu input -> output
- `src/dsp/yin.rs`:
  - Detektor YIN
- `src/dsp/scale.rs`:
  - Quantization do skali muzycznej
- `src/dsp/smoothing.rs`:
  - Wygładzanie ratio (retune speed)
- `src/dsp/phase_vocoder.rs`:
  - FFT-based pitch shifter
- `src/dsp/psola.rs`:
  - TD-PSOLA pitch shifter (czasowo-zsynchronizowany z okresem glosu)
- `src/dsp/formant.rs`:
  - Podstawowe zachowanie obwiedni formantowej
- `src/dsp/processor.rs`:
  - Pełny pipeline DSP block-by-block

## Build

```bash
cargo build --release --bin rust_autotune_cli
```

## Build pluginu (VST3/CLAP)

Projekt ma wspolny rdzen DSP i moze byc budowany jako plugin przez feature `plugin`.

```bash
cargo build --release --features plugin --lib
```

Po buildzie artefakt biblioteki znajdziesz w katalogu `target/release/`.

Na Windows:

- `rust_autotune.dll` zawiera eksporty pluginowe CLAP i VST3 (NIH-plug).

Typowa instalacja do testow:

- CLAP: skopiuj i zmien rozszerzenie na `.clap`, potem umiesc w `%COMMONPROGRAMFILES%/CLAP/`.
- VST3: umiesc biblioteke wewnatrz bundla `.vst3` (lub uzyj narzedzia bundlujacego NIH-plug w kolejnym kroku).

## Uruchomienie

```bash
cargo run --release --bin rust_autotune_cli -- \
  --block-size 256 \
  --scale major \
  --root C \
  --strength 1.0 \
  --aggressiveness 0.9 \
  --retune-ms 30 \
  --dry 0 \
  --wet 100 \
  --formant true
```

Przyklad bardziej naturalny:

```bash
cargo run --release --bin rust_autotune_cli -- --scale minor --root A --retune-ms 90 --strength 0.55 --aggressiveness 0.35 --dry 35 --wet 65
```

Przyklad bardziej robotyczny:

```bash
cargo run --release --bin rust_autotune_cli -- --scale chromatic --retune-ms 5 --strength 1.0 --aggressiveness 1.0 --dry 0 --wet 100
```

## Parametry

- `--block-size`: 128..512
- `--sample-rate`: opcjonalnie wymuszenie sample rate
- `--min-freq`, `--max-freq`: zakres detekcji YIN
- `--yin-threshold`: prog YIN
- `--confidence-threshold`: minimalna pewnosc do strojenia
- `--retune-ms`: czas wygładzania (mniejszy = szybciej)
- `--strength`: 0..1, ogolna sila korekcji wysokości
- `--aggressiveness`: 0..1, charakter snapu (0 = natural, 1 = hard tune)
- `--scale`: `chromatic|major|minor`
- `--root`: `C`, `D#`, `Bb` itd.
- `--dry`: 0..100 (%)
- `--wet`: 0..100 (%)
- `--formant`: wlacz/wyłącz formant correction
- `--formant-amount`: 0..1
- `--midi-note`: wymuszenie targetu MIDI (np. 69 = A4)

## Decyzje DSP

- YIN jest liczony na oknie 2048 próbek, co daje lepsza stabilnosc wokalu kosztem niewielkiej latencji analitycznej.
- TD-PSOLA pracuje na oknie 1024 probek i overlap x4, a dlugosc grainow jest powiazana z okresem glosu.
- Retune speed jest realizowany przez filtr one-pole na ratio, co ogranicza jitter i klikniecia.
- Strength kontroluje ile docelowego ratio trafia do toru pitch shiftingu.
- Aggressiveness decyduje, czy snap ma byc lagodny (natural) czy twardy (robotyczny).
- Dry/wet realizuje bezpieczne przejscie tonalne miedzy sygnalem oryginalnym i korekcja.

### Ostatnie poprawki produkcyjne (celowane, bez przepisywania calosci)

- YIN: dodane usuwanie DC i okno Hann przed obliczeniem CMNDF, co stabilizuje F0 i redukuje octave errors.
- Tracking F0: fallback do poprzedniego wiarygodnego pitch ma teraz limit czasu (histereza), aby unikac "zawieszania" zlej nuty.
- PSOLA pitch marks: markery analizy sa kotwiczone w centrum ramki i mapowane do markerow syntezy metoda najblizszego sasiedztwa, co poprawia ciaglosc fazy i naturalnosc.
- Mix dry/wet: tor dry jest wyrownany o latencje pitch shiftera i mieszany equal-power, co ogranicza comb filtering.

## Jak dziala PSOLA (krok po kroku)

Implementacja w [src/dsp/psola.rs](src/dsp/psola.rs) jest zoptymalizowana pod wokal i real-time:

1. Detekcja voiced/unvoiced:
  - Uzywa stabilnego F0 z toru detekcji oraz prostej kontroli ZCR.
  - Dla unvoiced fragmentow przechodzi plynnie w passthrough, co chroni transjenty.

2. Wyznaczanie pitch-markow (okresow):
  - Na podstawie okresu probkowego szukane sa lokalne piki amplitudy w oknach poszukiwania.
  - Pitch-marki tworza os czasu segmentow analitycznych.

3. Generowanie znacznikow syntezy:
  - Znaczniki syntezy sa rozstawiane wg nowego okresu (`period_s = period_a / ratio`).
  - To realizuje pitch shifting bez klasycznego phase-vocoderowego rozmycia formantow.

4. OLA zsynchronizowany z pitch-markami:
  - Kazdy grain jest okienkowany Hannem i dodawany do bufora wynikowego.
  - Rownolegle akumulowana jest suma wag okna, a wyjscie jest normalizowane przez te wage.
  - Zapobiega to modulacji amplitudy i efektowi "boxy/muffled".

5. Continuity i anti-click:
  - Crossfade voiced mix miedzy frame'ami usuwa klikniecia przy przejsciach voiced/unvoiced.
  - Ograniczanie skoku pitch-ratio i filtracja F0 redukuja warble/glitches.

## Sterowanie w pluginie

W hostach VST3/CLAP plugin udostepnia parametry:

- `Retune Speed`
- `Strength`
- `Aggressiveness`
- `Scale` (Chromatic/Major/Minor)
- `Key` (C..B)
- `Dry` (0..100%)
- `Wet` (0..100%)
- `Bypass`

## Ograniczenia

- To jest wersja CLI real-time, nie plugin VST3/AU.
- Formant correction to podstawowa metoda; zaawansowane modele (cepstrum/LPC warstwowe) mozna dodac jako kolejny etap.
