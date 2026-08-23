//! Self-test for the FM synth.  Programs a simple tone and dumps a WAV.
use std::fs::File;
use std::io::Write;
use pydmg_neogeo::audio::ym2610::Ym2610;

fn main() {
    let mut ym = Ym2610::new();
    // Program FM ch 0 (port A).
    // DT/MUL = 1 (mul=1)
    ym.write_port(0, 0x30); ym.write_port(1, 0x01); // op0
    ym.write_port(0, 0x34); ym.write_port(1, 0x01); // op1
    // TL = 0 (loudest) on op1 (carrier), 10 on op0 (modulator)
    ym.write_port(0, 0x40); ym.write_port(1, 0x18); // op0 TL = 24
    ym.write_port(0, 0x44); ym.write_port(1, 0x00); // op1 TL = 0
    // AR = 31 (fastest), DR=0, SR=0, SL=0, RR=15 (fastest release)
    ym.write_port(0, 0x50); ym.write_port(1, 0x1F); // op0 AR=31
    ym.write_port(0, 0x54); ym.write_port(1, 0x1F); // op1 AR=31
    ym.write_port(0, 0x80); ym.write_port(1, 0x0F); // op0 SL/RR
    ym.write_port(0, 0x84); ym.write_port(1, 0x0F); // op1 SL/RR
    // Algorithm 4 (parallel), pan L+R, fb=4
    ym.write_port(0, 0xB0); ym.write_port(1, (4u8 << 3) | 4);
    ym.write_port(0, 0xB4); ym.write_port(1, 0xC0); // pan L+R
    // FNUM = 0x269 (440 Hz-ish), block 4
    let fnum = 0x269u16; let block = 4u16;
    let fnum_block = (block << 11) | fnum;
    ym.write_port(0, 0xA4); ym.write_port(1, (fnum_block >> 8) as u8 & 0x3F);
    ym.write_port(0, 0xA0); ym.write_port(1, fnum_block as u8);
    // Key on ch 0, all slots
    ym.write_port(0, 0x28); ym.write_port(1, 0xF0);

    // Generate 1 second of audio
    let n = 55_555;
    let mut wav: Vec<i16> = Vec::with_capacity(n*2);
    for _ in 0..n {
        let (l, r) = ym.step_one_sample();
        wav.push(l); wav.push(r);
    }

    // Write WAV
    let mut f = File::create("/tmp/fm_test.wav").unwrap();
    let bytes = (wav.len() as u32) * 2;
    let chunk = 36 + bytes;
    f.write_all(b"RIFF").unwrap();
    f.write_all(&chunk.to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&55555u32.to_le_bytes()).unwrap();
    f.write_all(&(55555u32*4).to_le_bytes()).unwrap();
    f.write_all(&4u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&bytes.to_le_bytes()).unwrap();
    for s in &wav { f.write_all(&s.to_le_bytes()).unwrap(); }

    // Stats
    let peak = wav.iter().map(|&x| x.unsigned_abs()).max().unwrap_or(0);
    let nz = wav.iter().filter(|&&x| x != 0).count();
    println!("FM test: peak={} non-zero={}/{} ({}%)",
        peak, nz, wav.len(), 100*nz/wav.len());
}
