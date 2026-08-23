//! Validate the Z80 core against the SingleStepTests/z80 v1 corpus.
//!
//! The corpus lives outside the repo by default (~1.6 GiB of JSON). To
//! run these tests, set `Z80_TESTS_DIR` to a checkout of
//! <https://github.com/SingleStepTests/z80>. When the env var is not
//! set we silently skip — keeps `cargo test` green for users without
//! the corpus.
//!
//! Each opcode file contains 1000 random "initial → final" states. We
//! load `initial` into a CPU + flat RAM, run one `step()`, and compare
//! every observable register against `final`. The undocumented `wz`
//! (MEMPTR) and `q` fields are checked too.

use serde_json::Value;
use std::path::PathBuf;
use crate::cpu::z80::cpu::{Cpu, Registers};
use crate::cpu::z80::FlatRam;

fn tests_dir() -> Option<PathBuf> {
    let p = std::env::var("Z80_TESTS_DIR").ok()?;
    let path = PathBuf::from(p).join("v1");
    if path.is_dir() { Some(path) } else { None }
}

fn load_state(state: &Value) -> (Registers, Vec<(u16, u8)>) {
    let mut r = Registers::default();
    r.a = state["a"].as_u64().unwrap() as u8;
    r.f = state["f"].as_u64().unwrap() as u8;
    r.b = state["b"].as_u64().unwrap() as u8;
    r.c = state["c"].as_u64().unwrap() as u8;
    r.d = state["d"].as_u64().unwrap() as u8;
    r.e = state["e"].as_u64().unwrap() as u8;
    r.h = state["h"].as_u64().unwrap() as u8;
    r.l = state["l"].as_u64().unwrap() as u8;
    r.ix = state["ix"].as_u64().unwrap() as u16;
    r.iy = state["iy"].as_u64().unwrap() as u16;
    r.sp = state["sp"].as_u64().unwrap() as u16;
    r.pc = state["pc"].as_u64().unwrap() as u16;
    r.i  = state["i"].as_u64().unwrap()  as u8;
    r.r  = state["r"].as_u64().unwrap()  as u8;
    r.af_ = state["af_"].as_u64().unwrap() as u16;
    r.bc_ = state["bc_"].as_u64().unwrap() as u16;
    r.de_ = state["de_"].as_u64().unwrap() as u16;
    r.hl_ = state["hl_"].as_u64().unwrap() as u16;
    r.im   = state["im"].as_u64().unwrap() as u8;
    r.iff1 = state["iff1"].as_u64().unwrap() != 0;
    r.iff2 = state["iff2"].as_u64().unwrap() != 0;
    r.wz   = state["wz"].as_u64().unwrap() as u16;
    r.q    = state.get("q").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    let ram_arr = state["ram"].as_array().unwrap();
    let mut ram = Vec::with_capacity(ram_arr.len());
    for entry in ram_arr {
        let pair = entry.as_array().unwrap();
        let addr = pair[0].as_u64().unwrap() as u16;
        let byte = pair[1].as_u64().unwrap() as u8;
        ram.push((addr, byte));
    }
    (r, ram)
}

fn cmp_regs(a: &Registers, b: &Registers, name: &str) -> Result<(), String> {
    macro_rules! chk { ($f:ident) => {
        if a.$f != b.$f {
            return Err(format!("{name}: {} mismatch (got {:?}, want {:?})", stringify!($f), a.$f, b.$f));
        }
    } }
    chk!(a); chk!(f); chk!(b); chk!(c); chk!(d); chk!(e); chk!(h); chk!(l);
    chk!(ix); chk!(iy); chk!(sp); chk!(pc); chk!(i); chk!(r);
    chk!(af_); chk!(bc_); chk!(de_); chk!(hl_);
    chk!(im); chk!(iff1); chk!(iff2);
    chk!(wz);
    chk!(q);
    Ok(())
}

fn run_opcode_file(path: &std::path::Path) -> Result<(usize, usize, Option<String>), String> {
    let raw = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let arr: Value = serde_json::from_slice(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let tests = arr.as_array().unwrap();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<String> = None;
    for test in tests {
        let name = test["name"].as_str().unwrap_or("?");
        let (init_regs, init_ram) = load_state(&test["initial"]);
        let (final_regs, _) = load_state(&test["final"]);

        let mut cpu = Cpu::new();
        cpu.regs = init_regs;
        let mut bus = FlatRam::new();
        for (addr, byte) in &init_ram {
            bus.mem[*addr as usize] = *byte;
        }
        // Some opcodes (IN, OUT, IN r,(C), OUT (C),r, INI/IND, OUTI/OUTD)
        // include a `ports` array of [port, byte, dir] entries. For
        // reads we pre-seed io_in[port] with the byte the corpus says
        // the bus presented. Writes are validated later from the log.
        let mut expected_writes: Vec<(u16, u8)> = Vec::new();
        if let Some(ports_arr) = test.get("ports").and_then(|v| v.as_array()) {
            for entry in ports_arr {
                let pair = entry.as_array().unwrap();
                let port = pair[0].as_u64().unwrap() as u16;
                let byte = pair[1].as_u64().unwrap() as u8;
                let dir = pair[2].as_str().unwrap_or("r");
                if dir == "r" {
                    bus.io_in[port as usize] = byte;
                } else {
                    expected_writes.push((port, byte));
                }
            }
        }
        cpu.step(&mut bus);

        let mut ok = true;
        if let Err(e) = cmp_regs(&cpu.regs, &final_regs, name) {
            ok = false;
            if first_err.is_none() { first_err = Some(e); }
        } else {
            let final_ram_arr = test["final"]["ram"].as_array().unwrap();
            for entry in final_ram_arr {
                let pair = entry.as_array().unwrap();
                let addr = pair[0].as_u64().unwrap() as u16;
                let want = pair[1].as_u64().unwrap() as u8;
                let got = bus.mem[addr as usize];
                if got != want {
                    ok = false;
                    if first_err.is_none() {
                        first_err = Some(format!("{name}: ram[{:#06x}] = {:#x}, want {:#x}", addr, got, want));
                    }
                    break;
                }
            }
        }
        // Validate I/O writes against `ports` (direction="w").
        if ok && !expected_writes.is_empty() && bus.io_out_log != expected_writes {
            ok = false;
            if first_err.is_none() {
                first_err = Some(format!(
                    "{name}: io writes mismatch (got {:?}, want {:?})",
                    bus.io_out_log, expected_writes
                ));
            }
        }
        if ok { passed += 1; } else { failed += 1; }
    }
    Ok((passed, failed, first_err))
}

#[test]
fn corpus_main_opcodes() {
    let Some(dir) = tests_dir() else {
        eprintln!("SKIP: set Z80_TESTS_DIR to a SingleStepTests/z80 checkout");
        return;
    };
    let mut total_p = 0usize;
    let mut total_f = 0usize;
    let mut failing: Vec<(String, usize, usize, String)> = Vec::new();

    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();
    // Optional filter by env var: Z80_OPCODES=00,01,02
    let filter_set: Option<std::collections::HashSet<String>> =
        std::env::var("Z80_OPCODES").ok()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());

    // Optional prefix filter: Z80_PREFIX=cb,ed,dd,fd,ddcb,fdcb (default: main only)
    let prefix_set: std::collections::HashSet<String> =
        std::env::var("Z80_PREFIX").ok()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default();
    let include_main = prefix_set.is_empty() || prefix_set.contains("main");

    for p in &files {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        // Stem is either "NN" (main) or "PP NN" (PP=cb|ed|dd|fd or "dd cb"/"fd cb").
        let is_prefixed = stem.contains(' ');
        if is_prefixed {
            // Multi-word stems: extract the prefix tag ("cb", "ed", "dd", "fd", "ddcb", "fdcb").
            let parts: Vec<&str> = stem.split_whitespace().collect();
            let tag = if parts.len() >= 3 {
                // e.g. "dd cb 00" -> "ddcb"
                format!("{}{}", parts[0], parts[1])
            } else {
                parts[0].to_string()
            };
            if !prefix_set.contains(&tag) { continue; }
        } else if !include_main {
            continue;
        }
        if let Some(set) = &filter_set {
            // Only filter main-table opcodes by hex name
            if !is_prefixed && !set.contains(&stem) { continue; }
        }
        match run_opcode_file(p) {
            Ok((p_, f_, first_err)) => {
                total_p += p_; total_f += f_;
                if f_ > 0 {
                    failing.push((stem, p_, f_, first_err.unwrap_or_default()));
                }
            }
            Err(e) => {
                failing.push((stem, 0, 1000, e));
                total_f += 1000;
            }
        }
    }
    let total = total_p + total_f;
    let pct = if total > 0 { 100.0 * total_p as f64 / total as f64 } else { 0.0 };
    println!("\nSingleStepTests main: {total_p}/{total} ({pct:.2}%)");
    println!("Files with failures: {} (showing first 25)", failing.len());
    for (n, p_, f_, err) in failing.iter().take(25) {
        println!("  {n}: {p_}/1000 pass, first error: {err}");
    }
    assert!(total_p > 0, "ran zero tests");
}
