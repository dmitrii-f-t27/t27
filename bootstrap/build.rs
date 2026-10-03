//! Hard language guard: fail `cargo build` if Cyrillic appears in specs or unlisted docs.
//! See docs/nona-03-manifest/SOUL.md Law #1, architecture/ADR-004-language-policy.md, docs/T27-CONSTITUTION.md Article LANG-EN.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const ERR_HEAD: &str = "t27c LANGUAGE POLICY VIOLATION";

fn is_cyrillic(c: char) -> bool {
    matches!(c, '\u{0400}'..='\u{04ff}')
}

fn load_allowlist(root: &Path) -> HashSet<String> {
    let p = root.join("docs/.legacy-non-english-docs");
    let mut set = HashSet::new();
    let Ok(txt) = fs::read_to_string(&p) else {
        return set;
    };
    for line in txt.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if !line.is_empty() {
            set.insert(line.replace('\\', "/"));
        }
    }
    set
}

fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            collect_files(&p, ext, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(p);
        }
    }
}

fn scan_cyrillic(
    path: &Path,
    rel_posix: &str,
    allow: &HashSet<String>,
) -> Result<(), String> {
    if allow.contains(rel_posix) {
        return Ok(());
    }
    let content = fs::read_to_string(path).map_err(|e| {
        format!("{ERR_HEAD}: cannot read {rel_posix}: {e}\nSee docs/nona-03-manifest/SOUL.md Law #1.")
    })?;
    for (line_no, line) in content.lines().enumerate() {
        for (col, c) in line.chars().enumerate() {
            if is_cyrillic(c) {
                let snippet: String = line.chars().take(120).collect();
                return Err(format!(
                    "{ERR_HEAD}: Cyrillic character U+{:04X} ('{}') in file {}\n\
                     Location: line {}, column {}\n\
                     Snippet: {}\n\
                     Fix: use English only in first-party sources and docs.\n\
                     Docs: docs/nona-03-manifest/SOUL.md Law #1, architecture/ADR-004-language-policy.md, docs/T27-CONSTITUTION.md (LANG-EN).\n\
                     If this file is grandfathered non-English, add its repo-relative path to docs/.legacy-non-english-docs (Architect approval only).",
                    c as u32,
                    c,
                    rel_posix,
                    line_no + 1,
                    col + 1,
                    snippet
                ));
            }
        }
    }
    Ok(())
}

fn rel_from_root(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rerun_line(_manifest_dir: &Path, root: &Path, file: &Path) {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let flag = Path::new("..").join(rel);
    let s = flag.to_string_lossy().to_string();
    println!("cargo:rerun-if-changed={}", s);
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .expect("bootstrap crate must live one level below repo root")
        .to_path_buf();

    // Everything below enforces REPOSITORY policy -- no Cyrillic in repo-owned
    // Rust, the M5 freeze -- by reading files that live outside this package:
    // ../docs/.legacy-non-english-docs and the like. A published crate is
    // unpacked on its own, so `..` is a build directory and those files are not
    // there. The script then died with a bare "No such file or directory" and
    // took `cargo publish` with it, which is why t27c had never been published.
    //
    // Those checks are about this repository, not about the crate a downstream
    // user is compiling. Outside the repository they are skipped, and said so.
    let in_repo = root.join("Cargo.toml").is_file() && root.join("specs").is_dir();
    if !in_repo {
        println!(
            "cargo:warning=t27c: built outside the t27 repository; repository policy checks skipped"
        );
        println!("cargo:rerun-if-changed=build.rs");
        return;
    }

    let allow = load_allowlist(&root);

    // --- Bootstrap compiler sources: no Cyrillic in repo-owned Rust ---
    let boot_src = manifest_dir.join("src");
    if boot_src.is_dir() {
        let mut rs_files = Vec::new();
        collect_files(&boot_src, "rs", &mut rs_files);
        for path in &rs_files {
            let rel = rel_from_root(&root, path);
            if let Err(msg) = scan_cyrillic(path, &rel, &HashSet::new()) {
                panic!("{msg}");
            }
            rerun_line(&manifest_dir, &root, path);
        }
    }
    let boot_tests = manifest_dir.join("tests");
    if boot_tests.is_dir() {
        let mut rs_files = Vec::new();
        collect_files(&boot_tests, "rs", &mut rs_files);
        for path in &rs_files {
            let rel = rel_from_root(&root, path);
            if let Err(msg) = scan_cyrillic(path, &rel, &HashSet::new()) {
                panic!("{msg}");
            }
            rerun_line(&manifest_dir, &root, path);
        }
    }

    // --- .t27 / .tri under specs/: no Cyrillic ever (no allowlist) ---
    let specs = root.join("specs");
    if specs.is_dir() {
        let mut spec_files = Vec::new();
        collect_files(&specs, "t27", &mut spec_files);
        collect_files(&specs, "tri", &mut spec_files);
        for path in &spec_files {
            let rel = rel_from_root(&root, path);
            if let Err(msg) = scan_cyrillic(path, &rel, &HashSet::new()) {
                panic!("{msg}");
            }
            rerun_line(&manifest_dir, &root, path);
        }
    }

    // --- First-party Markdown (same rules as CI script) ---
    for dir in ["docs", "architecture", "clara-bridge", "conformance"] {
        let base = root.join(dir);
        if !base.is_dir() {
            continue;
        }
        let mut md_files = Vec::new();
        collect_files(&base, "md", &mut md_files);
        for path in md_files {
            let rel = rel_from_root(&root, &path);
            if let Err(msg) = scan_cyrillic(&path, &rel, &allow) {
                // W689: this was `eprintln!`. Cargo parses `cargo:` directives
                // from a build script's STDOUT only, so every Markdown language
                // violation was written to a stream nobody reads -- the check
                // ran, found the files, and reported to no one. Lesson 384 even
                // asserts these "emit build warnings"; they never did.
                println!("cargo:warning={msg}");
            }
            rerun_line(&manifest_dir, &root, &path);
        }
    }
    // specs/**/*.md
    let specs_md = root.join("specs");
    if specs_md.is_dir() {
        let mut md_files = Vec::new();
        collect_files(&specs_md, "md", &mut md_files);
        for path in md_files {
            let rel = rel_from_root(&root, &path);
            if let Err(msg) = scan_cyrillic(&path, &rel, &allow) {
                // W689: this was `eprintln!`. Cargo parses `cargo:` directives
                // from a build script's STDOUT only, so every Markdown language
                // violation was written to a stream nobody reads -- the check
                // ran, found the files, and reported to no one. Lesson 384 even
                // asserts these "emit build warnings"; they never did.
                println!("cargo:warning={msg}");
            }
            rerun_line(&manifest_dir, &root, &path);
        }
    }
    for name in [
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "TASK.md",
        "SOUL.md",
        "OWNERS.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "CODE_OF_CONDUCT.md",
    ] {
        let path = root.join(name);
        if path.is_file() {
            if let Err(msg) = scan_cyrillic(&path, name, &allow) {
                // W689: this was `eprintln!`. Cargo parses `cargo:` directives
                // from a build script's STDOUT only, so every Markdown language
                // violation was written to a stream nobody reads -- the check
                // ran, found the files, and reported to no one. Lesson 384 even
                // asserts these "emit build warnings"; they never did.
                println!("cargo:warning={msg}");
            }
            rerun_line(&manifest_dir, &root, &path);
        }
    }

    // --- FROZEN_HASH seal enforcement for bootstrap/src/compiler.rs ---
    // FROZEN.md §4.1: every cargo build must verify the frozen compiler surface.
    let frozen_path = manifest_dir.join("stage0").join("FROZEN_HASH");
    let compiler_path = manifest_dir.join("src").join("compiler.rs");
    let compiler_bytes = fs::read(&compiler_path).unwrap_or_else(|e| {
        panic!(
            "t27c FROZEN HASH violation: cannot read bootstrap/src/compiler.rs: {e}\n\
             See FROZEN.md and CANON.md M5."
        )
    });
    let live_hash = format!("{:x}", Sha256::digest(&compiler_bytes));
    let frozen_text = fs::read_to_string(&frozen_path).unwrap_or_else(|e| {
        panic!(
            "t27c FROZEN HASH violation: cannot read bootstrap/stage0/FROZEN_HASH: {e}\n\
             See FROZEN.md §4."
        )
    });
    let expected_hash = frozen_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .and_then(|line| line.split_whitespace().next());
    let expected_hash = expected_hash.unwrap_or_else(|| {
        panic!(
            "t27c FROZEN HASH violation: bootstrap/stage0/FROZEN_HASH has no operational line.\n\
             Expected format: <64-hex-sha256> <repo-relative-path>. See FROZEN.md §4."
        )
    });
    if live_hash != expected_hash {
        panic!(
            "t27c FROZEN HASH violation: bootstrap/src/compiler.rs has changed without a seal update.\n\
             Expected seal: {expected_hash}\n\
             Live hash:   {live_hash}\n\
             Run the freeze ceremony (M5) from bootstrap/: cargo run --release -- frozen-digest\n\
             Then copy the printed line into bootstrap/stage0/FROZEN_HASH.\n\
             See FROZEN.md §5 and CANON.md M5."
        );
    }
    rerun_line(&manifest_dir, &root, &frozen_path);
    rerun_line(&manifest_dir, &root, &compiler_path);

    // --- CREDIT_HASH seal enforcement for docs/T27-CONSTITUTION.md Article CREDIT ---
    // The article is entrenched: its text may only change together with its seal.
    let constitution_path = root.join("docs").join("T27-CONSTITUTION.md");
    let credit_seal_path = manifest_dir.join("stage0").join("CREDIT_HASH");
    let constitution = fs::read_to_string(&constitution_path).unwrap_or_else(|e| {
        panic!(
            "t27c CREDIT SEAL violation: cannot read docs/T27-CONSTITUTION.md: {e}\n\
             See docs/T27-CONSTITUTION.md Article CREDIT."
        )
    });
    let article = credit_article(&constitution).unwrap_or_else(|| {
        panic!(
            "t27c CREDIT SEAL violation: docs/T27-CONSTITUTION.md has no '## Article CREDIT' \
             heading, names it in more than one heading line, or could hide text from a reader \
             (raw HTML, an invisible character, a setext heading, a non-ASCII letter in a \
             heading). Exactly one line must read: {CREDIT_HEADING}. The article is \
             entrenched and may not be removed, duplicated or shadowed."
        )
    });
    let live_credit = format!("{:x}", Sha256::digest(article.as_bytes()));
    let sealed_credit = fs::read_to_string(&credit_seal_path).unwrap_or_else(|e| {
        panic!(
            "t27c CREDIT SEAL violation: cannot read bootstrap/stage0/CREDIT_HASH: {e}\n\
             See docs/T27-CONSTITUTION.md Article CREDIT."
        )
    });
    let sealed_credit = sealed_credit
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("");
    if live_credit != sealed_credit {
        panic!(
            "t27c CREDIT SEAL violation: Article CREDIT in docs/T27-CONSTITUTION.md differs from its seal.\n\
             Sealed: {sealed_credit}\n\
             Live:   {live_credit}\n\
             The article is entrenched. Change it only in a PR that also updates \
             bootstrap/stage0/CREDIT_HASH, quotes the owner's explicit approval and bumps \
             the charter version. See docs/T27-CONSTITUTION.md Article CREDIT."
        );
    }
    rerun_line(&manifest_dir, &root, &constitution_path);
    rerun_line(&manifest_dir, &root, &credit_seal_path);

    println!("cargo:rerun-if-changed=../docs/.legacy-non-english-docs");
    println!("cargo:rerun-if-changed=build.rs");
}

/// The heading of the entrenched article, compared as a whole line.
const CREDIT_HEADING: &str = "## Article CREDIT \u{2014} who is rewarded (entrenched)";

/// The sealed text of Article CREDIT: from its heading up to, not including, the
/// next level-1 or level-2 heading outside a code fence, or the end of the file.
/// A `---` rule does not end it: in Markdown that is a line, and text after it
/// still reads as part of the article. Only a line with at most three leading
/// spaces and no tab is a heading or a fence; deeper, it is an indented code
/// block and ends nothing. Carriage returns are dropped so CRLF seals the same
/// bytes.
///
/// The seal must cover what a reader sees, so the charter may not hide text or
/// disguise a heading at all: `None` (refused) if it carries raw HTML (a comment,
/// `<details>`, `<h2>`), an invisible formatting character, a setext heading, a
/// heading with a non-ASCII letter (a look-alike `I`), or more than one heading
/// line naming the article.
fn credit_article(doc: &str) -> Option<String> {
    let doc = doc.replace('\r', "");
    if hides_or_disguises(&doc) {
        return None;
    }
    let named = doc
        .lines()
        .filter(|l| markdown_lead(l).is_some_and(|t| t.starts_with('#')) && l.contains("Article CREDIT"))
        .count();
    if named != 1 {
        return None;
    }
    let mut out = String::new();
    let mut inside = false;
    let mut fence = false;
    for line in doc.split_inclusive('\n') {
        let bare = line.trim_end_matches('\n');
        if !inside {
            if bare == CREDIT_HEADING {
                inside = true;
                out.push_str(line);
            }
            continue;
        }
        let lead = markdown_lead(bare).unwrap_or("");
        if lead.starts_with("```") || lead.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && (lead.starts_with("# ") || lead.starts_with("## ")) {
            break;
        }
        out.push_str(line);
    }
    inside.then_some(out)
}

/// The line without its indent, if the indent is at most three spaces and holds
/// no tab: only such a line can open a heading or a fence in Markdown.
fn markdown_lead(line: &str) -> Option<&str> {
    let lead = line.trim_start_matches(' ');
    let indent = &line[..line.len() - lead.len()];
    (indent.len() <= 3 && !lead.starts_with('\t')).then_some(lead)
}

/// True if the charter could show a reader something other than its text: raw
/// HTML, an invisible formatting character, a setext heading, or a heading that
/// carries a non-ASCII letter.
fn hides_or_disguises(doc: &str) -> bool {
    const INVISIBLE: &[char] = &[
        '\u{00AD}', '\u{034F}', '\u{061C}', '\u{115F}', '\u{1160}', '\u{180E}', '\u{200B}',
        '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}',
        '\u{202D}', '\u{202E}', '\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}',
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{3164}', '\u{FEFF}', '\u{FFA0}',
    ];
    if doc.contains(INVISIBLE) {
        return true;
    }
    let raw_html = doc.as_bytes().windows(2).any(|w| {
        w[0] == b'<' && (w[1].is_ascii_alphabetic() || matches!(w[1], b'/' | b'!' | b'?'))
    });
    if raw_html {
        return true;
    }
    let mut prev_text = false;
    let mut fence = false;
    for line in doc.lines() {
        let lead = markdown_lead(line);
        let l = lead.unwrap_or("");
        if l.starts_with("```") || l.starts_with("~~~") {
            fence = !fence;
            prev_text = false;
            continue;
        }
        if fence {
            continue;
        }
        let underline = lead.is_some()
            && !l.trim_end().is_empty()
            && (l.trim_end().bytes().all(|b| b == b'=') || l.trim_end().bytes().all(|b| b == b'-'));
        if underline && prev_text {
            return true;
        }
        if l.starts_with('#') && line.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) {
            return true;
        }
        prev_text = !line.trim().is_empty() && !l.starts_with('#') && !underline;
    }
    false
}
