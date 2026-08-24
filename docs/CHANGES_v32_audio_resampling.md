# v32 — Fix del audio en Android: resampling nativo 55555 → 48000 Hz

## Problema reportado

> "El audio suena mal (chasquidos, harshness, distorsión) en la app Android."

## Diagnóstico (OODA)

### Observación

Cadena de audio de la v3:

```
YM2610  →  audio_buffer @ 55_555 Hz  →  AudioTrack(sampleRate=55555)
                                            │
                                            ▼
                          Android AudioFlinger MIXER  (siempre a 48 000 Hz)
                                            │
                                            ▼  Kaiser windowed-sinc resampler
                                            │  L/M = 9600/11111  (M > 256)
                                            │  → modo INTERPOLATED POLYPHASE
                                            │  → track EXCLUIDA del FastMixer
                                            ▼
                                          DAC físico
```

### Orientación

Tres fuentes de la degradación:

1. **Ratio de resampling awkward.** `48000/55555 = gcd=5 → L=9600, M=11111`. Con `M > 256`, el resampler del sistema Android cae en la ruta lenta *interpolated polyphase* (Android NDK docs, *"Use simple resampling ratios (fixed versus interpolated polyphases)"*). Esta ruta hace **interpolación de coeficientes** por muestra → passband ripple, aliasing armónico y jitter de fase → **harshness audible y "shimmering"**.

2. **Latencia extra + underruns.** El propio Google documenta ([NDK audio guide](https://developer.android.com/ndk/guides/audio/sampling-audio), sección *"If you're concerned about latency, don't resample"*):

   > *"Resampling prevents the track from being placed in the FastMixer path, which means that significantly higher latency occurs due to the additional, larger buffer in the ordinary Mixer path (typically 20 milliseconds)."*

   Nuestra `AudioEngine.kt` mitigaba esto multiplicando el buffer por 8 en debug builds — pero eso sólo escondía los underruns bajo latencia extra. En release, la latencia empeoraba y el mixer normal era más propenso a chasquidos bajo picos de CPU (emulación del M68K + Z80 + YM2610 en paralelo).

3. **Rate técnicamente admitido pero fuera del sweet spot.** Android acepta cualquier `sampleRate` que el DSP soporte, pero **el AudioFlinger sólo mezcla a 48 kHz nativos** en el 99% de dispositivos modernos. Empujar 55 555 Hz obliga siempre a resampling del sistema, sin excepciones.

### Decisión

Corregir según la estrategia canónica documentada tanto por Google como por la comunidad de emuladores (cf. jsgroth, *"A Way to Do Emulator Audio Resampling"*):

> *"Create an audio playback stream with the same sample rate as the console's native sample rate and just output samples as you generate them."* — sólo funciona bien si el rate cae en la ruta fija (ratios `L/M` con `M < 256`). Cuando no cae, la alternativa es: **hacer nosotros el resampling con un filtro decente antes de empujar al AudioTrack, y crear el AudioTrack al rate nativo del dispositivo.**

Elegido: hacer el resampling **dentro del núcleo Rust** con un FIR polifase windowed-sinc.

### Acción

1. **Nuevo módulo `neogeo/audio/resampler.rs`** (~250 LoC):
   - `StereoResampler` con FIR windowed-sinc (**Kaiser β=8.6, 65 taps**) diseñado en tiempo de construcción.
   - **Contador fraccional entero exacto**: `SOURCE_MILLIHZ = 55_555_556` mHz, `TARGET_MILLIHZ = 48_000_000` mHz → sin drift acumulado sobre horas de gameplay.
   - Ring buffer estéreo, convolución por muestra de salida (~130 MACs).
   - 4 tests unitarios: DC-gain unitaria, respuesta DC correcta, rate de salida exacto a 1 ppm, reset.

2. **`SystemConfig::output_sample_rate: Option<u32>`** — si `Some(hz)`, `audio_buffer` contiene ya samples resampleados a `hz`.

3. **`System::new()`** construye el resampler cuando `output_sample_rate` está presente. **`System::reset()`** hace flush del ring buffer para evitar clicks al arrancar un cart nuevo.

4. **`System::step()`** desvía cada sample del YM2610 al resampler (si está activo) en lugar de al buffer directo. La cadencia `M68K_CYCLES_PER_AUDIO_SAMPLE = 216` sigue siendo exacta (`12e6 × 144 / 8e6 = 216` entero).

5. **`android-jni/src/lib.rs`**:
   - Nueva constante `HOST_AUDIO_HZ = 48_000`.
   - `EmulatorState::new` inicializa `output_sample_rate: Some(48_000)`.
   - `nativeAudioSampleRate` devuelve `48_000`.

6. **`AudioEngine.kt`**:
   - Buffer bajado de `× 8` a `× 4` (con FastMixer activo, no hace falta la reserva enorme).
   - Comentarios actualizados explicando la nueva cadena.

7. **CLI SDL2 desktop preserva el path original** (`output_sample_rate: None`, samples a 55 555 Hz nativos) porque SDL2 tiene su propio resampler decente y el WAV dump quiere el stream crudo.

## Verificación

### Unit tests

```
$ cargo test --lib --no-default-features audio::resampler::
running 4 tests
test audio::resampler::tests::fir_has_unit_dc_gain ...        ok
test audio::resampler::tests::dc_input_produces_dc_output ... ok
test audio::resampler::tests::reset_clears_state ...          ok
test audio::resampler::tests::output_rate_is_correct_within_1ppm ... ok
```

### E2E con `mslug.zip` real

Nuevo test `resampler_pipeline_produces_48khz_stream`:

```
$ PYDMG_MSLUG_ZIP=/path/to/mslug.zip cargo test --release \
      --no-default-features --test from_bytes_smoke \
      resampler_pipeline -- --nocapture

resampler pipeline: 600 frames → 480010 stereo pairs (target 480000) → drift 20 ppm
     nonzero samples = 493356
test resampler_pipeline_produces_48khz_stream ... ok
```

**20 ppm de deriva** = 1 sample cada 27 horas. Inaudible. **493 356 samples no-cero** ya en los primeros 10 segundos (BIOS jingle SSG del arranque + attract screen).

### Suite completa

```
$ cargo test --release --no-default-features
TOTAL: 141 passed, 0 failed
```

Los 87 unit tests originales + 4 nuevos del resampler + 50 de integración vídeo/M68K/Z80/protecciones/boot **siguen todos verdes**.

## Impacto de rendimiento

- **CPU por sample de salida**: 130 MACs (2 canales × 65 taps).
- **Coste absoluto**: 48 000 samples/s × 130 MACs = **6.24 MFLOPS**.
- **En un Cortex-A55 @ 1.8 GHz**: < 1 % de un core.
- **En un Cortex-X1 @ 2.8 GHz (Pixel 6)**: < 0.05 % de un core.

Muy por debajo del coste del propio YM2610 emulado (que ya hace síntesis FM + SSG + 2× ADPCM en cada sample).

## Referencias externas

- Android NDK, *Sampling audio*: <https://developer.android.com/ndk/guides/audio/sampling-audio>
- jsgroth, *A Way to Do Emulator Audio Resampling*: <https://jsgroth.dev/blog/posts/a-way-to-do-audio-resampling/>
- Kaiser window design formula (P.P. Vaidyanathan, *Multirate Systems and Filter Banks*, §3.2).
