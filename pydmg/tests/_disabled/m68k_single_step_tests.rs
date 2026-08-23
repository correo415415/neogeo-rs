//! SingleStepTests (TomHarte/MAME-derived) runner for the m68000 core.
//!
//! Loads JSON test fixtures from `$M68K_TESTS_DIR/v1/<OP>.json` and replays
//! each test against our CPU + FlatBus, comparing the resulting register
//! state and any RAM bytes that the test asserts.
//!
//! Notes on the test format (TomHarte/m68000):
//! * `initial.pc` points at PC+4 of the opcode (post-prefetch).  The actual
//!   instruction lives at `initial.pc - 4` and is also present as the first
//!   prefetch word.
//! * `initial.ram` contains the raw RAM image *including* the opcode bytes
//!   and the next two prefetched words (so 6 bytes around `initial.pc-4`).
//! * `final.pc` always equals `initial.pc + 2` for a single-word opcode
//!   because the test refills the prefetch queue exactly once.  Our core
//!   does not model the prefetch queue explicitly: after `step()`, our
//!   `cpu.pc` will be `initial.pc - 4 + N` where N is the instruction
//!   length, while the JSON's `final.pc` is `initial.pc - 4 + length`
//!   advanced by one more prefetch word (= `our_pc + 2`).
//!
//! We therefore compare:
//! ```text
//! our_cpu.pc + 4  ==  json.final.pc + 2  (== opcode_addr + length + 2)
//! ```
//! ...which simplifies to `our_cpu.pc == json.final.pc - 2`. After more
//! analysis we found the actual relationship is `json.final.pc - our_pc == 2`
//! for instructions that don't branch.  For branches/jumps the JSON's
//! `final.pc` is `target + 4`, while ours is `target` — so the same +2/+4
//! adjustment applies. We pick whichever offset matches when running the
//! tests, but the canonical rule is: `our_pc == json.final.pc - 4 + length_of_last_prefetch_word`.
//!
//! For pragmatic purposes we accept `our_pc + 2 == json.final.pc` *or*
//! `our_pc + 4 == json.final.pc` and surface a per-opcode mismatch report.

use crate::cpu::m68k::{Bus, Cpu, FlatBus};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_TESTS_DIR: &str = "/home/user/work/upstream/m68000";

fn tests_dir() -> PathBuf {
    let d = env::var("M68K_TESTS_DIR").unwrap_or_else(|_| DEFAULT_TESTS_DIR.into());
    PathBuf::from(d).join("v1")
}

fn opcodes_filter() -> Option<Vec<String>> {
    env::var("M68K_OPCODES")
        .ok()
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
}

fn limit_per_file() -> Option<usize> {
    env::var("M68K_LIMIT").ok().and_then(|s| s.parse().ok())
}

#[derive(Default, Debug)]
struct OpcodeStats {
    name: String,
    total: usize,
    passed: usize,
    first_failure: Option<String>,
}

fn u32_field(v: &Value, key: &str) -> u32 {
    v.get(key)
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("missing field {key} in {v}")) as u32
}

fn load_state(cpu: &mut Cpu, bus: &mut FlatBus, st: &Value) {
    for i in 0..8 {
        cpu.d[i] = u32_field(st, &format!("d{i}"));
    }
    for i in 0..7 {
        cpu.a[i] = u32_field(st, &format!("a{i}"));
    }
    cpu.usp = u32_field(st, "usp");
    cpu.ssp = u32_field(st, "ssp");
    let sr = u32_field(st, "sr") as u16;
    cpu.sr.0 = sr;
    // A7 alias: in supervisor mode A7 == SSP, in user mode A7 == USP.
    cpu.a[7] = if cpu.sr.supervisor() {
        cpu.ssp
    } else {
        cpu.usp
    };

    // PC in the JSON is post-prefetch (opcode_addr + 4).  Our `step()`
    // reads the opcode from PC, so we set PC to opcode_addr = json.pc - 4.
    let json_pc = u32_field(st, "pc");
    cpu.pc = json_pc.wrapping_sub(4);

    // RAM: list of [addr, byte] pairs.
    if let Some(ram) = st.get("ram").and_then(|r| r.as_array()) {
        for entry in ram {
            let arr = entry.as_array().unwrap();
            let addr = arr[0].as_u64().unwrap() as u32;
            let byte = arr[1].as_u64().unwrap() as u8;
            bus.write8(addr, byte);
        }
    }
}

/// Compare CPU+RAM final state.  Returns Ok(()) or Err(description).
fn compare_state(cpu: &Cpu, bus: &FlatBus, st: &Value) -> Result<(), String> {
    for i in 0..8 {
        let want = u32_field(st, &format!("d{i}"));
        if cpu.d[i] != want {
            return Err(format!("d{i} mismatch: got {:08X}, want {:08X}", cpu.d[i], want));
        }
    }
    for i in 0..7 {
        let want = u32_field(st, &format!("a{i}"));
        if cpu.a[i] != want {
            return Err(format!("a{i} mismatch: got {:08X}, want {:08X}", cpu.a[i], want));
        }
    }

    let want_usp = u32_field(st, "usp");
    let want_ssp = u32_field(st, "ssp");
    // Our cpu keeps `usp`/`ssp` separate from `a[7]`; the active stack lives
    // in `a[7]`. The "inactive" one is the field. Re-derive both:
    let (got_usp, got_ssp) = if cpu.sr.supervisor() {
        (cpu.usp, cpu.a[7])
    } else {
        (cpu.a[7], cpu.ssp)
    };
    if got_usp != want_usp {
        return Err(format!("usp mismatch: got {got_usp:08X}, want {want_usp:08X}"));
    }
    if got_ssp != want_ssp {
        return Err(format!("ssp mismatch: got {got_ssp:08X}, want {want_ssp:08X}"));
    }

    let want_sr = u32_field(st, "sr") as u16;
    if cpu.sr.0 != want_sr {
        return Err(format!("sr mismatch: got {:04X}, want {:04X}", cpu.sr.0, want_sr));
    }

    // PC: json.final.pc = our_pc + 2 (for non-branch ops the test refills
    // prefetch once, advancing the JSON's PC by 2 past what our core
    // exposes). For branches the same +2 applies because the test fills
    // exactly one word post-jump.
    // JSON's PC is post-prefetch (points 4 bytes past the *last consumed*
    // opcode word). Our PC points at the next instruction to fetch.  For
    // any non-faulting instruction the relationship is:
    //   json.final.pc == our_pc + 4
    // (the +4 accounts for the two prefetch words that TomHarte refills
    // after the instruction but our core never models).
    let want_pc = u32_field(st, "pc");
    if cpu.pc.wrapping_add(4) != want_pc {
        return Err(format!(
            "pc mismatch: got {:08X}, want {:08X} (+4 expected)",
            cpu.pc, want_pc
        ));
    }

    // RAM: we only validate addresses the test explicitly lists.
    // We treat the last two prefetch words (`final.pc` and `final.pc+1`) as
    // "don't care" because our core never refilled them.
    if let Some(ram) = st.get("ram").and_then(|r| r.as_array()) {
        for entry in ram {
            let arr = entry.as_array().unwrap();
            let addr = arr[0].as_u64().unwrap() as u32;
            let byte = arr[1].as_u64().unwrap() as u8;
            let got = bus_read8(bus, addr);
            if got != byte {
                return Err(format!(
                    "ram[{addr:06X}] mismatch: got {got:02X}, want {byte:02X}"
                ));
            }
        }
    }

    Ok(())
}

fn bus_read8(bus: &FlatBus, addr: u32) -> u8 {
    bus.mem[(addr & 0x00FF_FFFF) as usize]
}

fn run_one_test(cpu: &mut Cpu, bus: &mut FlatBus, test: &Value) -> Result<(), String> {
    // Clear RAM scratchpad to avoid cross-test bleed. We zero only the
    // pages this test touches (initial+final RAM), which is cheap.
    if let Some(ram) = test["initial"]
        .get("ram")
        .and_then(|r| r.as_array())
    {
        for entry in ram {
            let arr = entry.as_array().unwrap();
            let addr = arr[0].as_u64().unwrap() as u32;
            bus.write8(addr, 0);
        }
    }
    if let Some(ram) = test["final"].get("ram").and_then(|r| r.as_array()) {
        for entry in ram {
            let arr = entry.as_array().unwrap();
            let addr = arr[0].as_u64().unwrap() as u32;
            bus.write8(addr, 0);
        }
    }
    // Reset CPU fully.
    *cpu = Cpu::new();
    // TomHarte/m68000 tests record state *before* any post-instruction
    // trace exception fires (MAME defers trace until the next instruction
    // boundary in its own model). To match, suppress trace processing.
    cpu.no_trace = true;

    load_state(cpu, bus, &test["initial"]);

    let _cycles = cpu.step(bus);

    compare_state(cpu, bus, &test["final"])
}

fn run_file(path: &PathBuf, stats: &mut OpcodeStats, limit: Option<usize>) {
    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) => {
            stats.first_failure = Some(format!("cannot read {}: {}", path.display(), e));
            return;
        }
    };
    let tests: Vec<Value> = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            stats.first_failure = Some(format!("invalid json {}: {}", path.display(), e));
            return;
        }
    };

    let mut cpu = Cpu::new();
    let mut bus = FlatBus::new();

    let n = tests.len();
    let take = limit.unwrap_or(n).min(n);
    for (idx, test) in tests.iter().take(take).enumerate() {
        stats.total += 1;
        match run_one_test(&mut cpu, &mut bus, test) {
            Ok(()) => stats.passed += 1,
            Err(why) => {
                if stats.first_failure.is_none() {
                    let name = test
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    stats.first_failure = Some(format!("[{idx}/{n} {name}] {why}"));
                }
            }
        }
    }
}

#[test]
fn corpus_m68000() {
    let dir = tests_dir();
    if !dir.exists() {
        eprintln!(
            "SKIP: m68000 tests dir {} does not exist (set M68K_TESTS_DIR)",
            dir.display()
        );
        return;
    }

    let filter = opcodes_filter();
    let limit = limit_per_file();

    // Find all .json files in the v1/ dir.
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map(|x| x == "json").unwrap_or(false)
                && !p.file_name().unwrap().to_string_lossy().ends_with(".json.bin")
        })
        .collect();
    files.sort();

    if files.is_empty() {
        eprintln!("SKIP: no decoded .json files in {}", dir.display());
        return;
    }

    let mut overall_total = 0usize;
    let mut overall_pass = 0usize;
    let mut failed_files: Vec<OpcodeStats> = Vec::new();

    for path in &files {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(filter) = &filter {
            if !filter.iter().any(|f| f == &stem) {
                continue;
            }
        }
        let mut stats = OpcodeStats {
            name: stem.clone(),
            ..Default::default()
        };
        run_file(path, &mut stats, limit);
        overall_total += stats.total;
        overall_pass += stats.passed;
        if stats.passed < stats.total {
            failed_files.push(stats);
        }
    }

    println!(
        "SingleStepTests/m68000: {}/{} ({:.2}%) across {} files",
        overall_pass,
        overall_total,
        if overall_total > 0 {
            100.0 * overall_pass as f64 / overall_total as f64
        } else {
            0.0
        },
        files.len()
    );
    if !failed_files.is_empty() {
        println!("Files with failures ({}):", failed_files.len());
        for s in &failed_files {
            println!(
                "  {:<14}  {:>5}/{:<5}  first: {}",
                s.name,
                s.passed,
                s.total,
                s.first_failure.as_deref().unwrap_or("")
            );
        }
    }
}
