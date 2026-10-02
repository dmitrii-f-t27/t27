// bitwalk -- walk a 7-series .bit with the rules of specs/xilinx7/{packets,frames}.t27 and nothing else.
// I/O and loops only; every decision is a function of the generated module.
//
//   bitwalk FILE...                         report packets, CRC checks, FDRI, frame ECC, COR0, IDCODE
//   bitwalk --frame FILE [MIN]              print the sparsest frame with a nonzero ECC and >= MIN nonzero words
//   bitwalk --cor0 N [--no-reseal] IN OUT   rewrite COR0's OSCFSEL, re-seal the CRC words
//
// Build (packets.rs and frames.rs are generated, never committed):
//   t27c gen-rust specs/xilinx7/packets.t27 > specs/xilinx7/packets.rs
//   t27c gen-rust specs/xilinx7/frames.t27 > specs/xilinx7/frames.rs
//   rustc -O --edition 2021 specs/xilinx7/bitwalk.rs -o specs/xilinx7/bitwalk
#[allow(unused_parens, dead_code, non_snake_case)]
#[path = "packets.rs"]
mod p;
#[allow(unused_parens, dead_code, non_snake_case)]
#[path = "frames.rs"]
mod f;

use std::collections::BTreeMap;

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
}

fn word(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn walk(b: &mut [u8], cor0_new: Option<u32>, reseal: bool, min_nz: usize) -> Walk {
    let sync = p::SYNC.to_be_bytes();
    let sync_at = b.windows(4).position(|w| w == sync).expect("no sync word");
    let mut r = Walk { sync_at, checks: 0, bad: 0, fdri: 0, cor0: None, idcode: None,
                       cmds: vec![], writes: BTreeMap::new(), patched: 0,
                       ecc_frames: 0, ecc_bad: 0, ecc_nonzero: 0, ecc_data: 0, sparse: None };
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

fn report(name: &str, r: &Walk) {
    let frames = r.fdri / p::WORDS_PER_FRAME as u64;
    let whole = p::whole_frames(r.fdri as u32);
    let cor0 = r.cor0.map(|c| format!("0x{:08X} (OSCFSEL {})", c, p::cor0_oscfsel(c))).unwrap_or("-".into());
    let id = r.idcode.map(|c| format!("0x{:08X}", c)).unwrap_or("-".into());
    println!("{name}\n  sync @ byte {}  CRC checks {} (bad {})  FDRI {} words = {} frames (whole: {})\n  COR0 {}  IDCODE {}  CMD {:?}",
             r.sync_at, r.checks, r.bad, r.fdri, frames, whole, cor0, id, r.cmds);
    println!("  ECC frames {} (bad {}; with data {}, nonzero ECC {})", r.ecc_frames, r.ecc_bad, r.ecc_data, r.ecc_nonzero);
    if let Some(c) = r.cor0 {
        let f: Vec<String> = (0..p::COR0_FIELDS.len())
            .map(|k| format!("{}={}", p::COR0_FIELDS[k], p::field(c, p::COR0_HI[k], p::COR0_LO[k])))
            .collect();
        println!("  COR0 fields: {}", f.join(" "));
    }
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
