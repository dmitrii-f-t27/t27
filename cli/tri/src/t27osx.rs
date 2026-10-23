//! `tri t27os status | coverage | shot`: processes and files only; every rule and text is specs/t27os/{status,fn_coverage,shot}.t27.
use crate::{t27fc as fc, t27sh as sh, t27st as st};
use std::process::{Command, Stdio};
fn out(c: &str, a: &[&str]) -> Vec<u8> { Command::new(c).args(a).stderr(Stdio::null()).output().map(|o| o.stdout).unwrap_or_default() }
fn trim(mut v: Vec<u8>) -> Vec<u8> { while v.last().is_some_and(|b| b.is_ascii_whitespace()) { v.pop(); } v }
fn specs() -> Vec<(String, Vec<u8>)> { let mut v: Vec<_> = std::fs::read_dir("specs/t27os").map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "t27")).map(|p| (p.display().to_string(), std::fs::read(&p).unwrap_or_default())).collect()).unwrap_or_default(); v.sort(); v }
pub fn status() -> i32 {
    let (b, c) = (trim(out("git", &["rev-parse", "--abbrev-ref", "HEAD"])), trim(out("git", &["rev-parse", "--short", "HEAD"])));
    let build = String::from_utf8_lossy(&trim(out("/usr/libexec/PlistBuddy", &["-c", "Print CFBundleVersion", "target/t27os/T27OS.app/Contents/Info.plist"]))).parse().unwrap_or(0u32);
    let dirty = !out("git", &["status", "--porcelain"]).is_empty();
    let s = specs(); let tests: u32 = s.iter().map(|(_, t)| st::st_count_tests(t, t.len())).sum();
    let tags = out("gh", &["release", "list", "-R", "dmitrii-f-t27/t27", "--limit", "40", "--json", "tagName", "-q", ".[].tagName"]);
    let mut buf = vec![0u8; 2048]; let n = st::st_text(&mut buf, 2048, &b, b.len(), &c, c.len(), build, dirty, tests, s.len() as u32, &tags, tags.len());
    print!("{}", String::from_utf8_lossy(&buf[..n.min(2048)])); 0
}
pub fn coverage() -> i32 {
    let (mut below, mut cov, mut tot, mut buf) = (0u32, 0u32, 0u32, vec![0u8; 512]);
    for (p, t) in specs() {
        let (mut m, mut tm) = (vec![0u8; t.len()], vec![0u8; t.len()]); fc::fc_code_mask(&t, t.len(), &mut m);
        if fc::fc_has_extern(&t, t.len(), &m) { continue }
        fc::fc_test_mask(&t, t.len(), &m, &mut tm); let (c, n) = (fc::fc_covered_count(&t, t.len(), &m, &tm), fc::fc_pub_fn_count(&t, t.len(), &m));
        below += (c < n) as u32; cov += c; tot += n; let w = fc::fc_line(&mut buf, 512, 0, p.as_bytes(), p.len(), c, n); print!("{}", String::from_utf8_lossy(&buf[..w.min(512)]));
    }
    let w = fc::fc_line(&mut buf, 512, 0, b"total", 5, cov, tot); print!("{}", String::from_utf8_lossy(&buf[..w.min(512)])); fc::fc_exit(below)
}
pub fn shot(panel: &str, file: &str, home: &str) -> i32 {
    let p = sh::sh_panel(panel.as_bytes(), panel.len()); if p == sh::SH_NONE { eprintln!("{}", sh::SH_USAGE); return 2 }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()); let (mut k, mut v, mut o, mut h) = ([0u8; 64], [0u8; 64], [0u8; 256], [0u8; 256]);
    let (kn, vn, on, hn) = (sh::sh_env_key(&mut k, 64, p), sh::sh_env_val(&mut v, 64, p), sh::sh_out_name(&mut o, 256, p, t), sh::sh_home(&mut h, 256, t));
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned(); let file = if file.is_empty() { s(&o[..on]) } else { file.to_string() }; let home = if home.is_empty() { s(&h[..hn]) } else { home.to_string() };
    let pty = std::fs::canonicalize(sh::SH_PTY_EXE).map(|x| x.display().to_string()).unwrap_or_default(); let pids = || s(&out("pgrep", &["-f", &pty])).split_whitespace().map(String::from).collect::<Vec<_>>(); let before = pids();
    if std::fs::create_dir_all(&home).is_err() { eprintln!("tri t27os shot: cannot make {home}"); return 2 }
    let Ok(mut app) = Command::new(sh::SH_APP_EXE).env("HOME", &home).env(s(&k[..kn]), s(&v[..vn])).env("T27OS_QUIET_JOBS", "1").stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { eprintln!("tri t27os shot: no {} (make t27os)", sh::SH_APP_EXE); return 2 };
    std::thread::sleep(std::time::Duration::from_secs(sh::SH_WAIT_S)); let ok = Command::new("screencapture").args(["-x", &file]).status().is_ok_and(|x| x.success());
    let _ = app.kill(); let _ = app.wait(); for q in pids().into_iter().filter(|q| !before.contains(q)) { let _ = Command::new("kill").arg(&q).status(); }
    if ok { println!("{file}"); 0 } else { eprintln!("tri t27os shot: screencapture failed"); 1 }
}
