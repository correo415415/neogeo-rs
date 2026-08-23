//! Neo Geo ROM-set loader.
//!
//! Supports three flavours:
//!   1. A **directory** containing the split ROM files (`.p1`, `.s1`, `.m1`,
//!      `.cX`, `.vX`, plus an optional BIOS such as `uni-bios_2_3.rom`).
//!   2. A **MAME/FBNeo `.zip`** romset (`mslug.zip`, `kof98.zip`, …).
//!   3. Just a single P-ROM file (`.bin` / `.rom`) for quick sanity tests.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RomError {
    #[error("no P-ROM found in romset")]
    MissingPRom,
    #[error("BIOS file not found at {0}")]
    BiosMissing(PathBuf),
    #[error("BIOS too large ({0} bytes, max 524288)")]
    BiosTooLarge(usize),
}

#[derive(Debug, Default)]
pub struct Cartridge {
    pub name: String,
    pub p_rom: Vec<u8>,
    pub s_rom: Vec<u8>,
    pub m_rom: Vec<u8>,
    /// V-ROMs paired with their **original filename** so `install_v_roms`
    /// can route them to the ADPCM-A or ADPCM-B address space based on the
    /// MAME/FBNeo convention (`*.v1*` = ADPCM-A, `*.v2*` = ADPCM-B, plain
    /// `*.v` / single-file V-ROM = shared blob for both).
    pub v_roms: Vec<(String, Vec<u8>)>,
    pub c_roms: Vec<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct RomSet {
    pub bios: Vec<u8>,
    pub cart: Cartridge,
    /// Y-zoom lookup table (000-lo.lo) — 64 KiB ROM that maps
    /// `(zoom_y << 8) | zoom_line` → `(tile_index << 4) | sprite_y`.
    /// Used by the sprite renderer for vertical scaling.
    pub lo_rom: Vec<u8>,
    /// BIOS fix-layer S-ROM (`sfix.sfix`, 128 KiB).
    ///
    /// Kept separate from `cart.s_rom` so the renderer can honour the
    /// HC259 Q5 ("use_cart_audio" / "fix source" multiplexer): when Q5=0
    /// the LSPC fix layer reads from the BIOS SFIX, when Q5=1 from the
    /// cart S-ROM. MAME models this via `set_fixed_layer_source()`.
    /// When the cart set does not bundle its own S-ROM, the loader
    /// copies `bios_sfix` into `cart.s_rom` as a fallback so that older
    /// code paths still work — the renderer will treat them as
    /// equivalent in that case.
    pub bios_sfix: Vec<u8>,
}

/// Sorted bucket of files keyed by lowercase name.
type Bucket = Vec<(String, Vec<u8>)>;

#[derive(Default)]
struct CategorisedFiles {
    p: Bucket,
    s: Bucket,
    m: Bucket,
    v: Bucket,
    c: Bucket,
    bios_candidates: Bucket,
    /// 000-lo.lo — hardware Y-zoom lookup ROM, plucked from the zip.
    lo_rom: Vec<u8>,
}

impl RomSet {
    /// Load a raw BIOS file (`.bin` / `.rom` / `.sp1`). Up to 512 KiB
    /// (Universe BIOS 4.0 is 512 KiB).
    pub fn load_bios(&mut self, path: &Path) -> Result<()> {
        let data = fs::read(path).map_err(|_| RomError::BiosMissing(path.to_path_buf()))?;
        if data.len() > 0x80000 {
            return Err(RomError::BiosTooLarge(data.len()).into());
        }
        self.bios = data;
        log::info!("Loaded BIOS '{}' ({} bytes)", path.display(), self.bios.len());
        Ok(())
    }

    /// One-call loader that figures out the input type automatically.
    pub fn load_cart_any(&mut self, path: &Path) -> Result<()> {
        let md = fs::metadata(path)
            .with_context(|| format!("cannot stat {}", path.display()))?;
        if md.is_dir() {
            self.load_cart_dir(path)
        } else {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "zip" => self.load_cart_zip(path),
                _ => self.load_cart_single_file(path),
            }
        }
    }

    /// Load a MAME-style **parent BIOS set** (e.g. `neogeo.zip`) on top of
    /// the current ROM state. Collects every BIOS candidate, the Y-zoom
    /// table (`000-lo.lo`) and the fallback fix / audio helpers
    /// (`sfix.sfix`, `sm1.sm1`) that ship in the parent set.
    ///
    /// Call this **before** `load_cart_zip` / `load_cart_dir` so the
    /// subsequent cart load can pick the BIOS automatically (or via
    /// `--bios-name`) and so the cart's own ROMs win on the slot buckets.
    pub fn load_parent_bios_zip(&mut self, path: &Path) -> Result<()> {
        let f = File::open(path)
            .with_context(|| format!("opening parent BIOS zip {}", path.display()))?;
        let mut zip = zip::ZipArchive::new(f)
            .with_context(|| format!("reading parent BIOS zip {}", path.display()))?;
        let mut bucket = CategorisedFiles::default();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let fname = entry.name().to_string();
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut data)?;
            categorise(fname, data, &mut bucket);
        }
        log::info!(
            "Parent BIOS set '{}': {} BIOS candidate(s), lo_rom={} bytes, fallback s={}, m={}",
            path.display(),
            bucket.bios_candidates.len(),
            bucket.lo_rom.len(),
            bucket.s.len(),
            bucket.m.len(),
        );
        // Auto-pick a BIOS if the user hasn't loaded one yet.
        if self.bios.is_empty() && !bucket.bios_candidates.is_empty() {
            if let Some((chosen_name, chosen_data)) = pick_bios(&mut bucket.bios_candidates) {
                log::info!(
                    "Auto-selecting BIOS '{}' ({} bytes) from parent set",
                    chosen_name,
                    chosen_data.len()
                );
                self.bios = chosen_data;
            }
        }
        if !bucket.lo_rom.is_empty() && self.lo_rom.is_empty() {
            log::info!(
                "Parent BIOS set provides Y-zoom table 000-lo.lo ({} bytes)",
                bucket.lo_rom.len()
            );
            self.lo_rom = bucket.lo_rom;
        }
        // Stash sfix.sfix / sm1.sm1 fallbacks. We keep them on the cart so
        // that, even if the cart set does NOT supply an s1 / m1, the BIOS
        // boot screen still has fix-tile graphics and the BIOS audio path
        // still has a Z80 program. The cart loader replaces these later if
        // the cart provides its own.
        // Capture sfix.sfix from the parent BIOS set into `bios_sfix`.
        // We keep a copy in `cart.s_rom` as a fallback only when the cart
        // doesn't supply its own — the renderer still inspects HC259 Q5
        // to decide which source to use frame-by-frame.
        if let Some((_, d)) = bucket.s.into_iter().next() {
            log::info!("BIOS SFIX captured: {} bytes (will respect HC259 Q5)", d.len());
            self.bios_sfix = d.clone();
            if self.cart.s_rom.is_empty() {
                log::info!("  cart.s_rom empty → falling back to BIOS SFIX for cart slot");
                self.cart.s_rom = d;
            }
        }
        if self.cart.m_rom.is_empty() {
            if let Some((_, d)) = bucket.m.into_iter().next() {
                log::info!("Using BIOS fallback audio M-ROM (sm1.sm1, {} bytes)", d.len());
                self.cart.m_rom = d;
            }
        }
        Ok(())
    }

    /// Explicit BIOS selection by *filename* inside an already-known parent
    /// set or cart zip. Lets the CLI pass `--bios-name uni-bios_4_0.rom`.
    pub fn pick_bios_from_zip(&mut self, path: &Path, wanted: &str) -> Result<()> {
        let f = File::open(path)
            .with_context(|| format!("opening zip {}", path.display()))?;
        let mut zip = zip::ZipArchive::new(f)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            if entry.name().eq_ignore_ascii_case(wanted) {
                let mut data = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut data)?;
                log::info!(
                    "Loading BIOS '{}' ({} bytes) from {}",
                    wanted,
                    data.len(),
                    path.display()
                );
                self.bios = data;
                return Ok(());
            }
        }
        anyhow::bail!("BIOS '{}' not found in {}", wanted, path.display());
    }

    /// Load a cartridge from a directory containing the split ROM files.
    pub fn load_cart_dir(&mut self, dir: &Path) -> Result<()> {
        let mut bucket = CategorisedFiles::default();
        let name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let entries: Vec<_> = fs::read_dir(dir)
            .with_context(|| format!("reading cart dir {}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        for ent in entries {
            let path = ent.path();
            if !path.is_file() {
                continue;
            }
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let data = fs::read(&path)?;
            categorise(fname, data, &mut bucket);
        }
        self.finalise_from_bucket(name, bucket)
    }

    /// Load a cartridge from a MAME/FBNeo zipped romset.
    pub fn load_cart_zip(&mut self, path: &Path) -> Result<()> {
        let f = File::open(path)
            .with_context(|| format!("opening zip {}", path.display()))?;
        let mut zip = zip::ZipArchive::new(f)
            .with_context(|| format!("reading zip {}", path.display()))?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let mut bucket = CategorisedFiles::default();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let fname = entry.name().to_string();
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut data)?;
            categorise(fname, data, &mut bucket);
        }
        self.finalise_from_bucket(name, bucket)
    }

    /// Load a single P-ROM file (no S/M/V/C, no BIOS). Useful for testing
    /// small homebrew programs.
    pub fn load_cart_single_file(&mut self, path: &Path) -> Result<()> {
        let data = fs::read(path)
            .with_context(|| format!("reading cart file {}", path.display()))?;
        let mut cart = Cartridge::default();
        cart.name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        cart.p_rom = data;
        log::info!(
            "Loaded standalone cart '{}' (P-ROM only, {} bytes)",
            cart.name,
            cart.p_rom.len()
        );
        self.cart = cart;
        Ok(())
    }

    fn finalise_from_bucket(&mut self, name: String, mut bucket: CategorisedFiles) -> Result<()> {
        // Auto-pick a BIOS if one is bundled in the zip and we haven't been
        // explicitly given one via --bios.
        if self.bios.is_empty() && !bucket.bios_candidates.is_empty() {
            if let Some((chosen_name, chosen_data)) = pick_bios(&mut bucket.bios_candidates) {
                log::info!(
                    "Auto-selecting BIOS '{}' ({} bytes) from zip",
                    chosen_name,
                    chosen_data.len()
                );
                self.bios = chosen_data;
            }
        }

        // Sort and concatenate.
        bucket.p.sort_by(|a, b| a.0.cmp(&b.0));
        bucket.v.sort_by(|a, b| a.0.cmp(&b.0));
        bucket.c.sort_by(|a, b| a.0.cmp(&b.0));

        // Save the Y-zoom table if we found it.
        if !bucket.lo_rom.is_empty() {
            log::info!("Loaded 000-lo.lo Y-zoom table: {} bytes", bucket.lo_rom.len());
            self.lo_rom = bucket.lo_rom;
        }

        if bucket.p.is_empty() {
            return Err(RomError::MissingPRom.into());
        }
        let mut cart = Cartridge { name, ..Default::default() };
        // SNK shipped some carts with a swapped P-ROM layout: the **first**
        // 1 MiB of the file actually maps to the second 1 MiB of the cart
        // bus, and the **second** 1 MiB of the file (which holds the vector
        // table and most of the code) maps to the first bus 1 MiB.
        //
        // MAME models this with:
        //   ROM_LOAD16_WORD_SWAP("201-p1.p1", 0x100000, 0x100000)
        //   ROM_CONTINUE(0x000000, 0x100000)
        //
        // We detect it by looking at a single 2 MiB P-ROM file: if its
        // first 1 MiB is mostly zeros and the second 1 MiB looks like a
        // valid 68k vector table (high byte of the first long looks like
        // an SSP in cart RAM, e.g. `$00xxxxxx`), we apply the swap.
        if bucket.p.len() == 1 && bucket.p[0].1.len() == 0x200000 {
            let (pname, mut d) = bucket.p.remove(0);
            // Check the very first 16 bytes ($0000-$000F = SSP+PC vector
            // in a normal cart). If those are zero but offset $100000
            // contains a plausible vector (high byte 0x00 or 0x10
            // indicating a RAM/work-RAM SSP, and second longword starts
            // with 0xC0 for a BIOS-mapped reset), it's the MAME
            // ROM_CONTINUE swap pattern (mslug, mslug2, ...).
            let upper_has_code = !d[0x100000..0x100010].iter().all(|&b| b == 0);
            let lower_is_empty = d[0x000000..0x000010].iter().all(|&b| b == 0);
            log::debug!(
                "P-ROM swap check: file_size={}, lower_empty={}, upper_has_code={}, lower[0..16]={:02X?}, upper[0..16]={:02X?}",
                d.len(), lower_is_empty, upper_has_code,
                &d[0..16], &d[0x100000..0x100010],
            );
            if upper_has_code && lower_is_empty {
                log::info!(
                    "Cart '{}' has swapped P-ROM layout (MAME ROM_CONTINUE pattern) -- swapping 1 MiB halves",
                    pname
                );
                let mut swapped = vec![0u8; 0x200000];
                swapped[0x000000..0x100000].copy_from_slice(&d[0x100000..0x200000]);
                swapped[0x100000..0x200000].copy_from_slice(&d[0x000000..0x100000]);
                d = swapped;
            }
            cart.p_rom.append(&mut d);
        } else {
            for (_, mut d) in bucket.p {
                cart.p_rom.append(&mut d);
            }
        }
        // Pick the s-rom. We tagged BIOS-provided sfix.sfix fallbacks with
        // the name `~bios-sfix.sfix`, which sorts last; cart-supplied
        // `*.s1` entries naturally win.
        let mut s_list: Vec<(String, Vec<u8>)> = bucket.s.into_iter().collect();
        s_list.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((sname, d)) = s_list.into_iter().next() {
            log::debug!("Using S-ROM '{}' ({} bytes)", sname, d.len());
            cart.s_rom = d;
        }
        let mut m_list: Vec<(String, Vec<u8>)> = bucket.m.into_iter().collect();
        m_list.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((mname, d)) = m_list.into_iter().next() {
            log::debug!("Using M-ROM '{}' ({} bytes)", mname, d.len());
            cart.m_rom = d;
        }
        for (name, d) in bucket.v {
            cart.v_roms.push((name, d));
        }
        for (_, d) in bucket.c {
            cart.c_roms.push(d);
        }
        // Inherit any parent-BIOS-supplied fallbacks (sfix.sfix / sm1.sm1)
        // that were stashed on `self.cart` *before* we built this cart, so
        // sets that ship without their own s1 / m1 (every MAME split set)
        // still have the BIOS-side helpers available.
        if cart.s_rom.is_empty() && !self.cart.s_rom.is_empty() {
            log::info!(
                "Cart did not supply s_rom — inheriting BIOS fallback ({} bytes)",
                self.cart.s_rom.len()
            );
            cart.s_rom = std::mem::take(&mut self.cart.s_rom);
        }
        if cart.m_rom.is_empty() && !self.cart.m_rom.is_empty() {
            log::info!(
                "Cart did not supply m_rom — inheriting BIOS fallback ({} bytes)",
                self.cart.m_rom.len()
            );
            cart.m_rom = std::mem::take(&mut self.cart.m_rom);
        }
        log::info!(
            "Loaded cart '{}' — P:{} S:{} M:{} V:{} C:{}",
            cart.name,
            cart.p_rom.len(),
            cart.s_rom.len(),
            cart.m_rom.len(),
            cart.v_roms.iter().map(|(_, d)| d.len()).sum::<usize>(),
            cart.c_roms.iter().map(Vec::len).sum::<usize>(),
        );
        self.cart = cart;
        Ok(())
    }
}

/// Look at a filename like `201-p1.bin`, `mslug.s1`, `sfix.sfix`,
/// `uni-bios_2_3.rom`, `sp-s2.sp1` and decide which bucket it belongs to.
fn categorise(fname: String, data: Vec<u8>, out: &mut CategorisedFiles) {
    let n = fname.to_ascii_lowercase();
    // Sub-helpers
    let is_p_x = matches_slot(&n, &['p']);
    let is_s_x = matches_slot(&n, &['s']);
    let is_m_x = matches_slot(&n, &['m']);
    let is_v_x = matches_slot(&n, &['v']);
    let is_c_x = matches_slot(&n, &['c']);

    // BIOS candidates (loaded as a *file*, not a P-ROM):
    //   uni-bios_*.rom, asia-s3.rom, japan-j3.bin, sp-s.sp1, sp-s2.sp1,
    //   sp-e.sp1, sp-j2.sp1, sp1.jipan.1024, usa_2slt.bin, vs-bios.rom,
    //   sp-45.sp1, sp-1v1_3db8c.bin, neo-epo.bin
    let looks_like_bios = n.starts_with("uni-bios")
        || n == "asia-s3.rom"
        || n == "japan-j3.bin"
        || n.starts_with("sp-")           // sp-s.sp1, sp-s2.sp1, sp-45.sp1, sp-e.sp1, sp-j2.sp1
        || n.starts_with("sp1.")          // sp1.jipan.1024
        || n == "vs-bios.rom"
        || n == "usa_2slt.bin"
        || n == "neo-epo.bin"
        || n == "neodebug.rom"
        || n == "neopen.sp1";

    if looks_like_bios && data.len() <= 0x80000 {
        out.bios_candidates.push((fname, data));
        return;
    }

    // 000-lo.lo is the hardware Y-zoom lookup ROM — 64 KiB. We need it
    // for proper vertical sprite scaling.
    if n == "000-lo.lo" || n == "000-lo.bin" {
        out.lo_rom = data;
        return;
    }
    // `sfix.sfix` is the BIOS fix-tile S-ROM. When the cart's own `s_rom`
    // is absent (as in MAME split sets such as `mslug2.zip`, where the
    // BIOS-side fix data lives in a separate parent set `neogeo.zip`), the
    // BIOS still needs `sfix.sfix` to draw its boot screen and the
    // "INSERT COIN" prompt. Treat it as a fallback fix-tile source: push it
    // onto the `s` bucket with the alphabetically last name so any cart-
    // provided `s1` ROM takes precedence (buckets are sorted before use).
    if n == "sfix.sfix" {
        out.s.push(("~bios-sfix.sfix".to_string(), data));
        return;
    }
    // `sm1.sm1` is the BIOS audio (Z80) ROM. Same pattern.
    if n == "sm1.sm1" {
        out.m.push(("~bios-sm1.sm1".to_string(), data));
        return;
    }

    if is_p_x {
        out.p.push((fname, data));
    } else if is_s_x {
        out.s.push((fname, data));
    } else if is_m_x {
        out.m.push((fname, data));
    } else if is_v_x {
        out.v.push((fname, data));
    } else if is_c_x {
        out.c.push((fname, data));
    } else if n.ends_with(".bin") && out.p.is_empty() && data.len() >= 0x80000 {
        // Plain `.bin` and we haven't picked a P-ROM yet — treat as the program.
        out.p.push((fname, data));
    } else {
        log::trace!("ignored unclassified file '{fname}' ({} bytes)", data.len());
    }
}

/// Return true if `lower` looks like a Neo Geo cartridge ROM file whose
/// slot letter is in `letters` (e.g. `['p']` for program ROMs).
///
/// Filename conventions handled (covering every MAME / FBNeo set seen):
///
/// * **FBNeo / MAME modern** `<set>.<slot><n>` — e.g. `mslug.p1`,
///   `241-c4.c4`. Detected by 2-char extension `<letter><digit>`.
/// * **MAME banked-second-ROM** `<id>-<slot><n>.s<slot><n>` — e.g.
///   `241-p2.sp2` (banked 2 MiB program ROM half of Metal Slug 2).
///   3-char extension `<prefix><letter><digit>` plus matching stem suffix.
/// * **Legacy** `*-<slot><n>.bin` — e.g. `201-p1.bin`.
fn matches_slot(lower: &str, letters: &[char]) -> bool {
    let dot = match lower.rfind('.') {
        Some(d) => d,
        None => return false,
    };
    let stem = &lower[..dot];
    let ext = &lower[dot + 1..];
    let ext_bytes = ext.as_bytes();

    // Pattern A: 2-char extension <letter><digit>.
    //   Example: `mslug.p1`, `241-c4.c4`.
    if ext_bytes.len() == 2
        && letters.contains(&(ext_bytes[0] as char))
        && ext_bytes[1].is_ascii_digit()
    {
        return true;
    }

    // Pattern A': 3-char extension <letter><digit><digit>.
    //   Example: `021-v11.v11`, `021-v21.v21` (JoyJoy Kid and every other
    //   early SNK cart with paired ADPCM-A/B ROMs). MAME/FBNeo tag those
    //   with an extension matching the base name, so `v11` and `v21` both
    //   still start with the slot letter `v` and end with digits — they
    //   just have an extra digit for the ROM position within the region.
    if ext_bytes.len() == 3
        && letters.contains(&(ext_bytes[0] as char))
        && ext_bytes[1].is_ascii_digit()
        && ext_bytes[2].is_ascii_digit()
    {
        return true;
    }

    // Pattern B: 3-char extension <prefix><letter><digit> with stem also
    // ending in <slot><n> separated by `-`/`_`/`.`/start.
    if ext_bytes.len() == 3
        && letters.contains(&(ext_bytes[1] as char))
        && ext_bytes[2].is_ascii_digit()
    {
        let sb = stem.as_bytes();
        if sb.len() >= 2 {
            let last = sb[sb.len() - 1];
            let prev = sb[sb.len() - 2];
            if last.is_ascii_digit() && letters.contains(&(prev as char)) {
                let before = if sb.len() >= 3 { sb[sb.len() - 3] } else { b'-' };
                if matches!(before, b'-' | b'_' | b'.' | b'/' | b'\\') {
                    return true;
                }
            }
        }
    }

    // Pattern C: `*-<letter><digit>.bin`.
    if ext == "bin" {
        let sb = stem.as_bytes();
        if sb.len() >= 2 {
            let last = sb[sb.len() - 1];
            let prev = sb[sb.len() - 2];
            if last.is_ascii_digit() && letters.contains(&(prev as char)) {
                let before = if sb.len() >= 3 { sb[sb.len() - 3] } else { b'-' };
                if matches!(before, b'-' | b'_' | b'.' | b'/' | b'\\') {
                    return true;
                }
            }
        }
    }
    false
}

/// Choose the best BIOS from a list of candidates, in priority order.
fn pick_bios(candidates: &mut Bucket) -> Option<(String, Vec<u8>)> {
    // Priority: official MVS SNK BIOS first. The Universe BIOS boots into
    // its own setup/menu and relocates the BIOS routines (SYSTEM_IO,
    // SYSTEM_RETURN, ...) away from the documented SNK addresses, which
    // prevents the standard coin->title->game state machine from running
    // cleanly in an automated/headless run: no coin is counted, PLAYER_START
    // never fires, the game never reaches gameplay and therefore never
    // triggers BGM/SFX. The stock `asia-s3` MVS BIOS keeps the documented
    // layout (SYSTEM_IO = $C0044A) and drives coin/start/gameplay correctly,
    // so we prefer it. uni-bios variants remain as fallbacks.
    let priorities = [
        // Official MVS BIOSes with the documented SYSTEM_IO ($C0044A) layout.
        // `asia-s3.rom` and `sp-s3.sp1` are byte-identical Asia MV-S BIOS.
        "asia-s3.rom",
        "sp-s3.sp1",
        "sp-s2.sp1",
        "sp-s.sp1",
        // Other official MVS BIOSes.
        "japan-j3.bin",
        "sp1-j3.bin",
        "vs-bios.rom",
        "sp-e.sp1",
        "sp-j2.sp1",
        "sp-45.sp1",
        "usa_2slt.bin",
        "sp1-u3.bin",
        "sp1-u4.bin",
        "uni-bios_4_0.rom",
        "uni-bios_3_3.rom",
        "uni-bios_3_2.rom",
        "uni-bios_3_1.rom",
        "uni-bios_3_0.rom",
        "uni-bios_2_3.rom",
        "uni-bios_2_2.rom",
        "uni-bios_2_1.rom",
        "uni-bios_2_0.rom",
        "uni-bios_1_3.rom",
        "uni-bios_1_2.rom",
        "uni-bios_1_1.rom",
        "uni-bios_1_0.rom",
    ];
    for want in priorities {
        if let Some(pos) = candidates.iter().position(|(n, _)| n.eq_ignore_ascii_case(want)) {
            return Some(candidates.remove(pos));
        }
    }
    candidates.pop()
}

#[cfg(test)]
mod matches_slot_tests {
    use super::matches_slot;

    #[test]
    fn joyjoy_v_roms_are_v_slot() {
        // These are the exact filenames FBNeo emits for JoyJoy Kid; both
        // must classify as V-ROMs so ADPCM audio is loaded.
        assert!(matches_slot("021-v11.v11", &['v']));
        assert!(matches_slot("021-v21.v21", &['v']));
    }

    #[test]
    fn mslug_v_roms_still_work() {
        assert!(matches_slot("201-v1.v1", &['v']));
        assert!(matches_slot("201-v2.v2", &['v']));
    }

    #[test]
    fn program_p_rom_variants() {
        assert!(matches_slot("mslug.p1", &['p']));
        assert!(matches_slot("241-p1.bin", &['p']));
    }

    #[test]
    fn non_matching_files_are_rejected() {
        assert!(!matches_slot("021-c1.c1", &['v']));
        assert!(!matches_slot("sfix.sfix", &['v']));
        assert!(!matches_slot("000-lo.lo", &['v']));
    }
}
