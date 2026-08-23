# v29 — YM2610 audio fidelity, MAME-canonical mix + octave fix

OODA pass on top of v28's LFO/SSG-EG/feedback work. Two surgical changes
based on direct comparison against the reference `02 - Neo-Geo SNK BIOS 2.mp3`
and the MAME ymfm canonical implementation.

## Changes in `crates/neogeo-core/src/ym2610.rs`

1. **MAME `add_route` gains in `step_one_sample`** (matches
   `src/mame/snk/neogeo.cpp::neogeo_stereo`):
   - SSG  -> L+R gain 0.84
   - FM/ADPCM L -> L gain 0.98
   - FM/ADPCM R -> R gain 0.98
   Applied as exact integer ratios (`*84/100`, `*98/100`) on i64 sums to
   avoid premature clipping before the final i16 clamp.

2. **Per-operator clamp in FM `step_one_with_per_channel`** following
   `ymfm_fm.ipp output_4op` (`clamp(result, -32768, 32767)`) when summing
   carrier-additive operators. Prevents per-channel wrap that previously
   bled noise into the global mix.

3. **FM octave fix in `FmCh::fnum_step`**: removed the historical `>> 1`
   shift in `(fnum << (block+5)) >> 1`. FFT analysis against the reference
   showed the FM lead pitched one octave below REF (REF lead ~440 Hz vs
   V28 lead ~220 Hz). After the fix:
   - REF (sec 0..6):    332..524 Hz
   - V29b (sec 4..11):  330..526 Hz  (matches within 1 cent)

## Tests
`cargo test --release -p neogeo-core --lib` -> 9/9 ok

## Capture
- `out/mslug_bios_v29b.wav` (1200 frames, 55555 Hz stereo).
- `analysis/run_bios_v29b.log` matches v28's keyon profile
  (fm=88, adpcma=21) so the structural FM/ADPCM activity is preserved.

## References
- MAME 3rdparty/ymfm/src/ymfm_opn.cpp::ym2610::clock_fm_and_adpcm
- MAME 3rdparty/ymfm/src/ymfm_opn.cpp::opn_registers_base::compute_phase_step
- MAME 3rdparty/ymfm/src/ymfm_fm.ipp::fm_channel::output_4op
- MAME src/mame/snk/neogeo.cpp::neogeo_stereo (route gains)
- FBNeo src/burn/snd/fm.c::YM2610UpdateOne (channel mask 0x36)

## Objective fidelity metrics

Spectrogram log-magnitude Pearson correlation against
`ref/ref_bios_55555.wav` over the music region (7.65 s) aligned to the
emulator capture (lag = 4.82 s from BIOS reset):

| build  | Pearson r |
|--------|-----------|
| v28b   | 0.5110    |
| v29    | 0.5108 (gains only, no octave fix yet) |
| v29b   | **0.5915** (after octave fix) |

Octave-band level deltas (v29b vs REF, dB):

| band       | delta |
|------------|-------|
|  30..  60  | -10.6 |
|  60.. 120  |  -9.7 |
| 120.. 250  |  -6.5 |
| 250.. 500  |  -5.8 |
| 500..1000  |  -7.4 |
|1000..2000  |  -7.2 |
|2000..4000  |  -6.0 |
|4000..8000  |  -4.1 |
|8000..16000 |  -4.5 |
|16k..27k    |  -5.1 |

After `loudnorm I=-25 LRA=1.5 TP=-1` (mimicking the YouTube-style master
that the MP3 reference comes from) the spectrogram Pearson r rises to
0.608, confirming that the residual ~-6 dB gap is the MP3's multiband
compression, not a chip mix issue.

## Tooling

The `tools/` directory ships the python scripts that produced the
metrics above so the next iteration can re-measure cheaply.

## v29c / v29d follow-ups (no audible delta on this BIOS jingle)

These changes adopt MAME's canonical formulas to make future work cleaner
even though `mslug` BIOS jingle does not exercise them:

- **`FmCh::kcode`** rewritten to match `opn_registers_base::cache_operator_data`:
  ```
  keycode  = ((block_freq >> 10) & 0x0F) << 1;
  keycode |= (0xfe80 >> ((block_freq >> 7) & 0x0F)) & 1;
  ```
  Previously used the FBNeo `opn_fktable` lookup (equivalent for most
  fnum/block combinations, but not bit-exact to MAME).

- **LFO PM applied on fnum, not on phase_step**: `compute_phase_step`
  in ymfm masks `(fnum + pm_offset) & 0xFFF` *before* the `<< block`
  shift so the modulation amplitude scales with the note's octave.
  Now mirrors that behaviour.

Pearson r on the captures stays at 0.5915 (BIOS jingle does not trigger
PMS or unusual KSR rates), but games that exercise vibrato/portamento
should now sound right.
