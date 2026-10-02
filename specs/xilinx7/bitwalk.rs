// bitwalk -- walk a 7-series .bit with the rules of specs/xilinx7/{packets,frames,far}.t27 and nothing else.
// I/O and loops only; every decision is a function of the generated module.
//
//   bitwalk FILE...                         report packets, CRC checks, FDRI, frame ECC, FAR walk, COR0, IDCODE
//   bitwalk --pins TILES FILE               for each line "pin tile base frames offset words" of TILES, does
//                                           the tile carry data in the frames the FAR walk assigns it?
//   bitwalk --bits SEGS FILE                is every set bit of FILE a bit some tile's segbits name, at the
//                                           frame the FAR walk assigns? SEGS: "S type minor bit" segbits,
//                                           "T type base frames offset words shift" tiles (shift: alias)
//   bitwalk --frame FILE [MIN]              print the sparsest frame with a nonzero ECC and >= MIN nonzero words
//   bitwalk --cor0 N [--no-reseal] IN OUT   rewrite COR0's OSCFSEL, re-seal the CRC words
//   bitwalk --write FRAMES OUT --part_file part.yaml --part_name NAME [--source S] [--generator G]
//           [--date D] [--time T]           write FRAMES (prjxray .frames text) as a .bit: frames placed
//                                           by the FAR walk, ECC sealed, packets as packets.t27's SEQ;
//                                           the header fields default to xc7frames2bit's
//   bitwalk --frames BIT OUT                write BIT's nonzero frames as .frames text, addresses from
//                                           the FAR walk (the inverse of --write)
//
// Build (packets.rs and frames.rs are generated, never committed):
//   t27c gen-rust specs/xilinx7/packets.t27 > specs/xilinx7/packets.rs
//   t27c gen-rust specs/xilinx7/frames.t27 > specs/xilinx7/frames.rs
//   t27c gen-rust specs/xilinx7/far.t27 > specs/xilinx7/far.rs
//   rustc -O --edition 2021 specs/xilinx7/bitwalk.rs -o specs/xilinx7/bitwalk
#[allow(unused_parens, dead_code, non_snake_case)]
#[path = "packets.rs"]
mod p;
#[allow(unused_parens, dead_code, non_snake_case)]
#[path = "frames.rs"]
mod f;
#[allow(unused_parens, dead_code, non_snake_case)]
#[path = "far.rs"]
mod w;

use std::collections::{BTreeMap, HashMap, HashSet};

struct Walk {
    sync_at: usize,
    checks: u32,
    bad: u32,
    fdri: u64,
    cor0: Option<u32>,
    idcode: Option<u32>,
    cmds: Vec<u32>,
    writes: BTreeMap<u32, u64>,
    patched: u32,
    ecc_frames: u32,
    ecc_bad: u32,
    ecc_nonzero: u32,
    ecc_data: u32,
    // (frame number, nonzero (word, value) pairs, stored ECC) of the sparsest frame with a nonzero ECC
    sparse: Option<(u32, Vec<(u32, u32)>, u32)>,
    // every FDRI frame, in stream order
    frames: Vec<[u32; 101]>,
}

impl Walk {
    // far.t27's part number for the IDCODE this stream writes (PARTS if none or unknown).
    fn part(&self) -> u32 {
        self.idcode.map_or(w::PARTS, w::part_of_idcode)
    }
}

fn word(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn walk(b: &mut [u8], cor0_new: Option<u32>, reseal: bool, min_nz: usize) -> Walk {
    let sync = p::SYNC.to_be_bytes();
    let sync_at = b.windows(4).position(|w| w == sync).expect("no sync word");
    let mut r = Walk { sync_at, checks: 0, bad: 0, fdri: 0, cor0: None, idcode: None,
                       cmds: vec![], writes: BTreeMap::new(), patched: 0,
                       ecc_frames: 0, ecc_bad: 0, ecc_nonzero: 0, ecc_data: 0, sparse: None, frames: vec![] };
    let (mut ecc, mut w50, mut frame) = (0u32, 0u32, [0u32; 101]);
    let (mut reg, mut op, mut left) = (0u32, p::OP_NOP, 0u32);
    let (mut crc, mut crc_new) = (0u32, 0u32);
    let mut i = sync_at + 4;
    while i + 4 <= b.len() {
        let w = word(b, i);
        let in_payload = left > 0;
        if !in_payload {
            reg = p::reg_after(reg, w);
            op = p::op_after(op, w);
            left = p::left_after(0, w);
            i += 4;
            continue;
        }
        left = p::left_after(left, w);
        let is_write = op == p::OP_WRITE;
        if is_write {
            *r.writes.entry(reg).or_default() += 1;
            let mut w_new = w;
            if reg == p::REG_CRC {
                r.checks += 1;
                if !p::crc_ok(w, crc) { r.bad += 1; }
                if reseal { w_new = p::resealed(w, crc, crc_new); }
            } else if reg == p::REG_COR0 {
                r.cor0 = Some(w);
                if let Some(v) = cor0_new { w_new = p::cor0_with_oscfsel(w, v); }
            } else if reg == p::REG_IDCODE {
                r.idcode = Some(w);
            } else if reg == p::REG_CMD {
                r.cmds.push(w);
            } else if reg == p::REG_FDRI {
                let idx = (r.fdri % f::FRAME_WORDS as u64) as u32;
                frame[idx as usize] = w;
                ecc = f::ecc_after(idx, w, ecc);
                if idx == f::ECC_WORD { w50 = w; }
                if idx == f::LAST_WORD {
                    r.frames.push(frame);
                    r.ecc_frames += 1;
                    if !f::ecc_ok(w50, ecc) { r.ecc_bad += 1; }
                    if frame.iter().any(|&v| v != 0) { r.ecc_data += 1; }
                    let stored = f::ecc_stored(w50);
                    if stored != 0 {
                        r.ecc_nonzero += 1;
                        let nz: Vec<(u32, u32)> = (0..101u32).filter(|&k| frame[k as usize] != 0)
                            .map(|k| (k, frame[k as usize])).collect();
                        let fewer = r.sparse.as_ref().map_or(true, |s| nz.len() < s.1.len());
                        if nz.len() >= min_nz && fewer { r.sparse = Some((r.ecc_frames - 1, nz, stored)); }
                    }
                    ecc = 0;
                }
                r.fdri += 1;
            }
            if w_new != w {
                b[i..i + 4].copy_from_slice(&w_new.to_be_bytes());
                r.patched += 1;
            }
            crc = p::crc_after(reg, w, crc);
            crc_new = p::crc_after(reg, w_new, crc_new);
        }
        i += 4;
    }
    r
}

// Pad frames of a whole-part write starting at FAR 0: the ROW_PAD_FRAMES after each group.
fn pad_positions(part: u32) -> Vec<u32> {
    let (mut at, mut pads) = (0u32, vec![]);
    let g0 = w::first_group(part);
    for g in g0..g0 + w::PART_GROUPS[part as usize] {
        at += w::group_frames(g);
        for k in 0..w::ROW_PAD_FRAMES { pads.push(at + k); }
        at += w::ROW_PAD_FRAMES;
    }
    pads
}

fn report(name: &str, r: &Walk) {
    let frames = r.fdri / p::WORDS_PER_FRAME as u64;
    let whole = p::whole_frames(r.fdri as u32);
    let cor0 = r.cor0.map(|c| format!("0x{:08X} (OSCFSEL {})", c, p::cor0_oscfsel(c))).unwrap_or("-".into());
    let id = r.idcode.map(|c| format!("0x{:08X}", c)).unwrap_or("-".into());
    println!("{name}\n  sync @ byte {}  CRC checks {} (bad {})  FDRI {} words = {} frames (whole: {})\n  COR0 {}  IDCODE {}  CMD {:?}",
             r.sync_at, r.checks, r.bad, r.fdri, frames, whole, cor0, id, r.cmds);
    println!("  ECC frames {} (bad {}; with data {}, nonzero ECC {})", r.ecc_frames, r.ecc_bad, r.ecc_data, r.ecc_nonzero);
    let part = r.part();
    if part == w::PARTS {
        println!("  FAR walk: IDCODE {} is not in far.t27's part table", id);
        return;
    }
    let walk = w::part_fdri_frames(part);
    let off = (walk as i64 - r.frames.len() as i64).unsigned_abs();
    let dirty = pad_positions(part).iter()
        .filter(|&&k| r.frames.get(k as usize).map_or(false, |fr| fr.iter().any(|&v| v != 0))).count();
    println!("  FAR walk {} frames (FDRI {}, off by {}; pad frames with data {})", walk, r.frames.len(), off, dirty);
    if let Some(c) = r.cor0 {
        let f: Vec<String> = (0..p::COR0_FIELDS.len())
            .map(|k| format!("{}={}", p::COR0_FIELDS[k], p::field(c, p::COR0_HI[k], p::COR0_LO[k])))
            .collect();
        println!("  COR0 fields: {}", f.join(" "));
    }
}

// Every address of part p, in FDRI order; pads are None.
fn walk_addresses(part: u32) -> Vec<Option<u32>> {
    let mut out = vec![];
    let g0 = w::first_group(part);
    for g in g0..g0 + w::PART_GROUPS[part as usize] {
        let first = w::first_column(g);
        let cols = w::GROUP_COLS[g as usize];
        let mut a = w::group_start(w::GROUP_KEY[g as usize]);
        while a != w::NO_FRAME {
            out.push(Some(a));
            a = w::far_next_in_row(a, w::COL_FRAMES[(first + w::far_column(a)) as usize], cols);
        }
        for _ in 0..w::ROW_PAD_FRAMES {
            out.push(None);
        }
    }
    out
}

fn flag<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
    a.iter().position(|s| s == name).and_then(|i| a.get(i + 1)).map(|s| s.as_str())
}

fn bit_field(out: &mut Vec<u8>, key: u8, text: &str) {
    out.push(key);
    let n = text.len() + 1;
    out.extend_from_slice(&[(n >> 8) as u8, n as u8]);
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

// frames text -> .bit. Returns the number of frames rejected (no address of the part).
fn write_bit(a: &[String]) -> u32 {
    let (frames_path, out_path) = (&a[1], &a[2]);
    let yaml = std::fs::read_to_string(flag(a, "--part_file").expect("--part_file part.yaml")).unwrap();
    let part_name = flag(a, "--part_name").expect("--part_name NAME");
    let idcode = yaml
        .lines()
        .find_map(|l| l.trim().strip_prefix("idcode:"))
        .map(|v| {
            let v = v.trim();
            v.strip_prefix("0x").map_or_else(|| v.parse().unwrap(), |h| u32::from_str_radix(h, 16).unwrap())
        })
        .expect("idcode: in part file");
    let part = w::part_of_idcode(idcode);
    assert!(part != w::PARTS, "IDCODE 0x{idcode:08X} is not in far.t27's part table");
    let nframes = w::part_fdri_frames(part) as usize;
    let fw = f::FRAME_WORDS as usize;
    let mut data = vec![0u32; nframes * fw];
    let mut placed = vec![false; nframes];
    let (mut rejected, mut dup, mut short) = (0u32, 0u32, 0u32);
    for line in std::fs::read_to_string(frames_path).unwrap().lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (addr, words) = line.split_once(' ').expect("ADDR WORDS");
        let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).unwrap();
        let words: Vec<u32> = words.split(',').map(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).unwrap()).collect();
        if words.len() != fw {
            short += 1;
            continue;
        }
        let i = w::fdri_index(part, addr);
        if i == w::NO_FRAME {
            println!("  REJECT 0x{addr:08X}: not an address of this part");
            rejected += 1;
            continue;
        }
        let i = i as usize;
        if placed[i] {
            dup += 1;
            continue;
        }
        placed[i] = true;
        data[i * fw..(i + 1) * fw].copy_from_slice(&words);
    }
    for fr in data.chunks_mut(fw) {
        let mut ecc = 0u32;
        for (k, v) in fr.iter().enumerate() {
            ecc = f::ecc_after(k as u32, *v, ecc);
        }
        let e = f::ECC_WORD as usize;
        fr[e] = f::with_ecc(fr[e], ecc);
    }
    let mut words: Vec<u32> = vec![];
    let frame_words = data.len() as u32;
    for s in 0..p::SEQ_STEPS {
        for j in 0..p::step_words(s) {
            words.push(p::step_word(s, j, idcode, frame_words));
        }
        if p::step_kind(s) == p::STEP_FDRI {
            words.extend_from_slice(&data);
        }
    }
    let mut out: Vec<u8> = p::BIT_MAGIC.iter().map(|&b| b as u8).collect();
    let source = flag(a, "--source").map_or_else(
        || std::path::Path::new(frames_path).file_name().unwrap().to_string_lossy().into_owned(),
        |s| s.to_string(),
    );
    let generator = flag(a, "--generator").unwrap_or("bitwalk");
    bit_field(&mut out, b'a', &format!("{source};Generator={generator}"));
    bit_field(&mut out, b'b', part_name);
    bit_field(&mut out, b'c', flag(a, "--date").unwrap_or("2000/01/01"));
    bit_field(&mut out, b'd', flag(a, "--time").unwrap_or("00:00:00"));
    out.push(b'e');
    out.extend_from_slice(&((words.len() * 4) as u32).to_be_bytes());
    for v in &words {
        out.extend_from_slice(&v.to_be_bytes());
    }
    std::fs::write(out_path, &out).unwrap();
    let given = placed.iter().filter(|&&x| x).count();
    println!(
        "{out_path}: {} bytes, {} words, FDRI {} frames ({} from FRAMES, rejected {}, duplicate {}, wrong length {})",
        out.len(), words.len(), nframes, given, rejected, dup, short
    );
    rejected
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(|s| s.as_str()) == Some("--cor0") {
        let v: u32 = a[1].parse().expect("OSCFSEL 0..63");
        let reseal = !a.iter().any(|s| s == "--no-reseal");
        let files: Vec<&String> = a[2..].iter().filter(|s| !s.starts_with("--")).collect();
        let mut b = std::fs::read(files[0]).unwrap();
        let r = walk(&mut b, Some(v), reseal, 2);
        std::fs::write(files[1], &b).unwrap();
        println!("patched {} word(s), reseal={reseal}", r.patched);
        return;
    }
    if a.first().map(|s| s.as_str()) == Some("--write") {
        let rejected = write_bit(&a);
        std::process::exit(if rejected == 0 { 0 } else { 1 });
    }
    if a.first().map(|s| s.as_str()) == Some("--frames") {
        let mut b = std::fs::read(&a[1]).unwrap();
        let r = walk(&mut b, None, false, 2);
        let part = r.part();
        assert!(part != w::PARTS, "IDCODE not in far.t27's part table");
        let addrs = walk_addresses(part);
        let mut text = String::new();
        let mut n = 0;
        for (fr, addr) in r.frames.iter().zip(addrs.iter()) {
            let data = fr.iter().any(|&v| v != 0);
            if let (true, Some(addr)) = (data, addr) {
                let ws: Vec<String> = fr.iter().map(|v| format!("0x{v:08X}")).collect();
                text.push_str(&format!("0x{addr:08X} {}\n", ws.join(",")));
                n += 1;
            }
        }
        std::fs::write(&a[2], text).unwrap();
        println!("{}: {} frames with data", a[2], n);
        return;
    }
    if a.first().map(|s| s.as_str()) == Some("--pins") {
        let tiles = std::fs::read_to_string(&a[1]).unwrap();
        let mut b = std::fs::read(&a[2]).unwrap();
        let r = walk(&mut b, None, false, 2);
        let (mut hit, mut total) = (0, 0);
        for line in tiles.lines().filter(|l| !l.trim().is_empty()) {
            let t: Vec<&str> = line.split_whitespace().collect();
            let n: Vec<u32> = t[2..6].iter().map(|x| x.parse().unwrap()).collect();
            let (base, nfr, off, nw) = (n[0], n[1], n[2] as usize, n[3] as usize);
            let data = (0..nfr).map(|k| w::fdri_index(r.part(), base + k)).any(|i| {
                i != w::NO_FRAME && r.frames.get(i as usize).map_or(false, |fr| fr[off..off + nw].iter().any(|&v| v != 0))
            });
            total += 1;
            if data { hit += 1; } else { println!("  MISS {} {} base 0x{:08X}", t[0], t[1], base); }
        }
        println!("{} pins {}/{} land in frames with data", a[2], hit, total);
        return;
    }
    if a.first().map(|s| s.as_str()) == Some("--bits") {
        let segs = std::fs::read_to_string(&a[1]).unwrap();
        let mut b = std::fs::read(&a[2]).unwrap();
        let r = walk(&mut b, None, false, 2);
        let part = r.part();
        let mut bits: HashMap<&str, Vec<Vec<u32>>> = HashMap::new();
        let (mut known, mut window) = (HashSet::new(), HashSet::new());
        for line in segs.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            let n: Vec<u32> = t[2..].iter().map(|x| x.parse().unwrap()).collect();
            if t[0] == "S" {
                let by_minor = bits.entry(t[1]).or_default();
                if by_minor.len() <= n[0] as usize { by_minor.resize(n[0] as usize + 1, vec![]); }
                by_minor[n[0] as usize].push(n[1]);
                continue;
            }
            let (base, nfr, off, nw, shift) = (n[0], n[1], n[2], n[3], n[4]);
            let (no_type, no_bits): (Vec<Vec<u32>>, Vec<u32>) = (vec![], vec![]);
            let by_minor = bits.get(t[1]).unwrap_or(&no_type);
            for k in 0..nfr {
                let i = w::fdri_index(part, base + k);
                let fr = match r.frames.get(i as usize) { Some(fr) if i != w::NO_FRAME => fr, _ => continue };
                let quiet = fr[off as usize..(off + nw) as usize].iter().all(|&v| v == 0);
                if quiet { continue; }
                for wd in off..off + nw { window.insert((i, wd)); }
                for &bit in by_minor.get(k as usize).unwrap_or(&no_bits) {
                    let inside = bit >= shift && bit < shift + nw * 32;
                    if inside { known.insert((i, off * 32 + bit - shift)); }
                }
            }
        }
        let (mut total, mut unknown, mut outside) = (0u32, 0u32, 0u32);
        for (i, fr) in r.frames.iter().enumerate() {
            for (wd, &v0) in fr.iter().enumerate() {
                let mut v = if wd as u32 == f::ECC_WORD { v0 & f::ECC_KEEP } else { v0 };
                while v != 0 {
                    let bit = v.trailing_zeros();
                    v &= v - 1;
                    total += 1;
                    let at = (i as u32, wd as u32);
                    if !window.contains(&at) { outside += 1; continue; }
                    if known.contains(&(at.0, at.1 * 32 + bit)) { continue; }
                    unknown += 1;
                    if unknown <= 5 { println!("  UNKNOWN frame {} word {} bit {}", at.0, at.1, bit); }
                }
            }
        }
        println!("{} bits {}/{} named by segbits (unknown {}, outside every tile {})",
                 a[2], total - unknown - outside, total, unknown, outside);
        return;
    }
    if a.first().map(|s| s.as_str()) == Some("--frame") {
        let mut b = std::fs::read(&a[1]).unwrap();
        let min_nz = a.get(2).map_or(2, |s| s.parse().expect("MIN nonzero words"));
        let r = walk(&mut b, None, false, min_nz);
        match r.sparse {
            Some((n, nz, stored)) => {
                println!("{} frame {} of the FDRI stream, stored ECC 0x{:04X}", a[1], n, stored);
                for (k, v) in nz { println!("  word {:3} = 0x{:08X}", k, v); }
            }
            None => println!("{}: no frame with a nonzero ECC", a[1]),
        }
        return;
    }
    for path in &a {
        let mut b = std::fs::read(path).unwrap();
        let t = std::time::Instant::now();
        let r = walk(&mut b, None, false, 2);
        report(path, &r);
        println!("  walked in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }
}
