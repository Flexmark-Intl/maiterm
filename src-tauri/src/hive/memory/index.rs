//! The index rebuild (docs/hive.md §9), pure: `MEMORY.md`'s text, the folder's files and Rust's
//! record in; the new text and what moves out of it back. `mod.rs` does the I/O.
//!
//! The file has two fenced sections. The kept block is the human's: never rewritten here (plan
//! D4). The index is maiTerm's: one line per memory file, from its frontmatter, newest first,
//! ending in Rust's provenance. Every other line is sorted: one that links a memory file is
//! absorbed into that file's line (Claude's own pointer lines), and the rest move, verbatim, to
//! `unfiled.md` (D5). Nothing is dropped but blank lines, duplicates of kept lines and maiTerm's
//! own lines.

use std::collections::BTreeSet;

use super::front::Front;
use super::record::MemoryRecord;

pub const KEPT_BEGIN: &str = "<!-- maiterm:kept:begin";
pub const KEPT_END: &str = "<!-- maiterm:kept:end";
pub const INDEX_BEGIN: &str = "<!-- maiterm:index:begin";
pub const INDEX_END: &str = "<!-- maiterm:index:end";
pub const KEPT_BEGIN_LINE: &str = "<!-- maiterm:kept:begin — human and pinned lines; maiTerm never rewrites these -->";
pub const KEPT_END_LINE: &str = "<!-- maiterm:kept:end -->";
pub const INDEX_BEGIN_LINE: &str = "<!-- maiterm:index:begin — generated from the files; edit the files, not these lines -->";
pub const INDEX_END_LINE: &str = "<!-- maiterm:index:end -->";

/// What Claude loads of `MEMORY.md`.
pub const CLAUDE_LINES: usize = 200;
pub const CLAUDE_BYTES: usize = 25_000;
/// The kept block's share of that.
pub const KEPT_MAX_LINES: usize = 60;
pub const KEPT_MAX_BYTES: usize = 8_000;

pub const MEMORY_MD: &str = "MEMORY.md";
pub const GENERATED_MD: &str = "MEMORY.generated.md";
pub const UNFILED_MD: &str = "unfiled.md";
pub const ARCHIVE_PREFIX: &str = "archive-";

pub const OVER_CAP: &str =
    "The kept block is over 60 lines or 8 KB: MEMORY.md wasn't rewritten; the full index is in MEMORY.generated.md";

const TITLE_MAX: usize = 80;
const HOOK_MAX: usize = 150;
const BY_MAX: usize = 60;

/// A top-level memory file as the writer listed it.
#[derive(Debug, Clone, Default)]
pub struct MemFile {
    pub name: String,
    pub front: Front,
    pub mtime_ms: i64,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rebuilt {
    /// None when nothing changed, or when it must not be written (the kept block over its cap).
    pub memory_md: Option<String>,
    /// The whole would-be file, when `MEMORY.md` couldn't take it.
    pub generated_md: Option<String>,
    /// Lines that move to `unfiled.md`, verbatim, in order.
    pub unfiled_append: Vec<String>,
    /// `(file name, content)` of the fold of the oldest lines.
    pub archive: Option<(String, String)>,
    /// maiTerm's archives nothing links any more.
    pub stale_archives: Vec<String>,
    pub status: Option<String>,
    /// The kept block on disk isn't the one Rust recorded; it was left as it is.
    pub kept_changed: bool,
    /// The index lines this text holds: the record's `generated`.
    pub generated: Vec<String>,
    /// Files with their own line in the index, folded into the archive, or linked from the kept block.
    pub indexed: Vec<String>,
    pub folded: Vec<String>,
    pub pinned: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Opts {
    /// Write `rec.kept` as the kept block whatever is on disk: the human's set, pin or restore.
    pub force_kept: bool,
}

/// `MEMORY.md` cut at its four fences.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    /// The kept block verbatim, its marker lines and line endings included.
    pub kept_raw: String,
    pub kept: Vec<String>,
    /// Every line outside the kept block, the index's included, in order.
    pub outside: Vec<String>,
}

/// Each line without its line ending, and the byte range of the whole line.
fn lines(text: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        out.push((raw.trim_end_matches(['\n', '\r']), at, at + raw.len()));
        at += raw.len();
    }
    out
}

/// None unless each marker is there exactly once, in order: then there are no fences.
pub fn split(text: &str) -> Option<Split> {
    let ls = lines(text);
    let find = |prefix: &str| -> Option<usize> {
        let mut hits = ls.iter().enumerate().filter(|(_, (l, _, _))| l.trim_start().starts_with(prefix)).map(|(i, _)| i);
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    };
    let (kb, ke, ib, ie) = (find(KEPT_BEGIN)?, find(KEPT_END)?, find(INDEX_BEGIN)?, find(INDEX_END)?);
    if !(kb < ke && ke < ib && ib < ie) {
        return None;
    }
    let mut kept_raw = text[ls[kb].1..ls[ke].2].to_string();
    if !kept_raw.ends_with('\n') {
        kept_raw.push('\n');
    }
    let kept = ls[kb + 1..ke].iter().map(|(l, _, _)| l.to_string()).collect();
    let outside = ls
        .iter()
        .enumerate()
        .filter(|(i, _)| !(kb..=ke).contains(i) && *i != ib && *i != ie)
        .map(|(_, (l, _, _))| l.to_string())
        .collect();
    Some(Split { kept_raw, kept, outside })
}

pub fn kept_block(kept: &[String]) -> String {
    let mut s = format!("{KEPT_BEGIN_LINE}\n");
    for l in kept {
        s.push_str(l);
        s.push('\n');
    }
    s.push_str(KEPT_END_LINE);
    s.push('\n');
    s
}

/// Bytes the kept lines take, each with its newline.
pub fn kept_bytes(kept: &[String]) -> usize {
    kept.iter().map(|l| l.len() + 1).sum()
}

pub fn kept_over_cap(kept: &[String]) -> bool {
    kept.len() > KEPT_MAX_LINES || kept_bytes(kept) > KEPT_MAX_BYTES
}

/// A fence marker in someone's text can never close a fence (as `layout::memory_seed` does).
pub fn defuse(s: &str) -> String {
    s.replace("<!--", "<! --").replace("-->", "-- >")
}

/// One line, no control characters, fences defused, at most `max` characters.
fn clean(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).collect();
    let flat = defuse(&flat);
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The `.md` files a line's markdown links name, when the link is one path component.
pub fn link_targets(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        let target = if let Some(r) = rest.strip_prefix('<') {
            r.split_once('>').map(|(t, _)| t)
        } else {
            rest.find([')', ' ', '\t']).map(|end| &rest[..end])
        };
        let Some(t) = target else { continue };
        let t = t.split('#').next().unwrap_or("");
        let t = t.strip_prefix("./").unwrap_or(t);
        if t.ends_with(".md") && !t.contains(['/', '\\']) && crate::hive::paths::is_component(t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Not indexed: the index itself, its overflow, and maiTerm's archives.
pub fn is_memory_file(name: &str, rec: &MemoryRecord) -> bool {
    name.ends_with(".md") && name != MEMORY_MD && name != GENERATED_MD && !rec.maiterm_files.iter().any(|m| m == name)
}

fn stem(name: &str) -> &str {
    name.strip_suffix(".md").unwrap_or(name)
}

/// `- [Title](file) — hook · by` (plan D2): every `·` in the hook becomes `-`, so the last ` · `
/// segment is always Rust's provenance and a description can't forge "· human".
pub fn index_line(f: &MemFile, rec: &MemoryRecord) -> String {
    let title = clean(f.front.name.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or(stem(&f.name)), TITLE_MAX).replace('[', "(").replace(']', ")");
    let hook = f.front.description.as_deref().map(|d| clean(&d.replace('·', "-"), HOOK_MAX)).filter(|d| !d.is_empty());
    let by = rec.files.get(&f.name).map(|p| p.by.as_str()).filter(|b| !b.is_empty()).unwrap_or(super::record::BY_UNKNOWN);
    let by = clean(&by.replace('·', "-"), BY_MAX);
    let link = if f.name.contains([' ', '(', ')']) && !f.name.contains(['<', '>']) { format!("<{}>", f.name) } else { f.name.clone() };
    match hook {
        Some(h) => format!("- [{title}]({link}) — {h} · {by}"),
        None => format!("- [{title}]({link}) · {by}"),
    }
}

/// `YYYY-MM-DD` (UTC) of epoch ms (Howard Hinnant's civil-from-days).
pub fn date_of_ms(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn count_lines(s: &str) -> usize {
    s.matches('\n').count()
}

pub fn rebuild(text: &str, files: &[MemFile], rec: &MemoryRecord, today: &str) -> Rebuilt {
    rebuild_with(text, files, rec, today, Opts::default())
}

pub fn rebuild_with(text: &str, files: &[MemFile], rec: &MemoryRecord, today: &str, opts: Opts) -> Rebuilt {
    let fences = split(text);
    let (kept_raw, kept, kept_changed) = match &fences {
        Some(s) if !opts.force_kept => (s.kept_raw.clone(), s.kept.clone(), s.kept != rec.kept),
        _ => (kept_block(&rec.kept), rec.kept.clone(), false),
    };
    let candidates: Vec<String> = match &fences {
        Some(s) => s.outside.clone(),
        None => lines(text).into_iter().map(|(l, _, _)| l.to_string()).collect(),
    };

    let mut memory: Vec<&MemFile> = files.iter().filter(|f| is_memory_file(&f.name, rec)).collect();
    memory.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then_with(|| a.name.cmp(&b.name)));
    let names: BTreeSet<&str> = memory.iter().map(|f| f.name.as_str()).collect();
    // A file the kept block links is pinned: reachable, so not listed twice.
    let pinned: BTreeSet<String> = kept.iter().flat_map(|l| link_targets(l)).filter(|t| names.contains(t.as_str())).collect();

    let mut unfiled_append: Vec<String> = Vec::new();
    for line in &candidates {
        let l = line.as_str();
        if l.trim().is_empty() || l.trim_start().starts_with("<!-- maiterm:") {
            continue;
        }
        if kept.iter().any(|k| k == l) || rec.kept.iter().any(|k| k == l) || rec.generated.iter().any(|g| g == l) {
            continue;
        }
        if link_targets(l).iter().any(|t| names.contains(t.as_str()) || rec.maiterm_files.contains(t)) {
            continue;
        }
        unfiled_append.push(l.to_string());
    }

    let entries: Vec<(&MemFile, String)> = memory.iter().filter(|f| !pinned.contains(&f.name)).map(|f| (*f, index_line(f, rec))).collect();
    let head = format!("{kept_raw}{INDEX_BEGIN_LINE}\n");
    let tail = format!("{INDEX_END_LINE}\n");
    let assemble = |index: &[String]| {
        let mut s = head.clone();
        for l in index {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str(&tail);
        s
    };
    let all: Vec<String> = entries.iter().map(|(_, l)| l.clone()).collect();
    let mut out = Rebuilt { kept_changed, pinned: pinned.iter().cloned().collect(), ..Rebuilt::default() };

    if kept_over_cap(&kept) {
        // Nothing moves out of a file that isn't rewritten, or it would be there twice.
        out.generated_md = Some(assemble(&all));
        out.status = Some(OVER_CAP.to_string());
        out.generated = rec.generated.clone();
        out.indexed = entries.iter().map(|(f, _)| f.name.clone()).collect();
        return out;
    }

    let base_lines = count_lines(&head) + count_lines(&tail);
    let base_bytes = head.len() + tail.len();
    let fits = |n: usize, bytes: usize| base_lines + n <= CLAUDE_LINES && base_bytes + bytes <= CLAUDE_BYTES;
    let bytes_of = |ls: &[String]| ls.iter().map(|l| l.len() + 1).sum::<usize>();
    let mut index = all.clone();
    let mut keep = all.len();
    if !fits(all.len(), bytes_of(&all)) {
        // The newest lines that fit beside one archive line; the rest fold into it (D3).
        let taken: BTreeSet<&str> = files.iter().map(|f| f.name.as_str()).filter(|n| !rec.maiterm_files.iter().any(|m| m == n)).collect();
        for k in (0..all.len()).rev() {
            let folded = &entries[k..];
            let date = |ms: i64| if ms > 0 { date_of_ms(ms) } else { today.to_string() };
            let newest = date(folded.iter().map(|(f, _)| f.mtime_ms).max().unwrap_or(0));
            let oldest = date(folded.iter().map(|(f, _)| f.mtime_ms).min().unwrap_or(0));
            let mut name = format!("{ARCHIVE_PREFIX}{newest}.md");
            let mut n = 2;
            // Never over an agent's own file of that name.
            while taken.contains(name.as_str()) {
                name = format!("{ARCHIVE_PREFIX}{newest}-{n}.md");
                n += 1;
            }
            let line = format!(
                "- [Older notes: {} files, {oldest} to {newest}]({name}) — every older memory file, one line each · {}",
                folded.len(),
                super::record::BY_MAITERM
            );
            if fits(k + 1, bytes_of(&all[..k]) + line.len() + 1) || k == 0 {
                let mut body = String::from(
                    "# Older memory files\n\nmaiTerm folded these out of MEMORY.md's index to keep it inside what Claude reads. Edit the files, not this list.\n\n",
                );
                for (_, l) in folded {
                    body.push_str(l);
                    body.push('\n');
                }
                index = all[..k].to_vec();
                index.push(line);
                out.archive = Some((name, body));
                keep = k;
                break;
            }
        }
    }
    out.indexed = entries[..keep].iter().map(|(f, _)| f.name.clone()).collect();
    out.folded = entries[keep..].iter().map(|(f, _)| f.name.clone()).collect();
    let current = out.archive.as_ref().map(|(n, _)| n.as_str());
    out.stale_archives = rec.maiterm_files.iter().filter(|m| Some(m.as_str()) != current).cloned().collect();
    out.unfiled_append = unfiled_append;
    let new_text = assemble(&index);
    out.generated = index;
    if new_text != text {
        out.memory_md = Some(new_text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::memory::front;
    use crate::hive::memory::record::FileProv;

    fn file(name: &str, title: &str, desc: &str, mtime: i64) -> MemFile {
        let text = format!("---\nname: {title}\ndescription: {desc}\n---\nbody\n");
        MemFile { name: name.into(), front: front::parse(&text), mtime_ms: mtime, size: text.len() as u64 }
    }

    fn rec_with(kept: &[&str]) -> MemoryRecord {
        MemoryRecord { v: 1, kept: kept.iter().map(|s| s.to_string()).collect(), ..MemoryRecord::default() }
    }

    fn seed() -> String {
        crate::hive::layout::memory_seed("Ship it")
    }

    /// Applies a rebuild as the writer would, for the next one to read.
    fn apply(text: &str, r: &Rebuilt, rec: &mut MemoryRecord) -> String {
        rec.generated = r.generated.clone();
        if let Some((n, _)) = &r.archive {
            if !rec.maiterm_files.contains(n) {
                rec.maiterm_files.push(n.clone());
            }
        }
        rec.maiterm_files.retain(|m| !r.stale_archives.contains(m));
        r.memory_md.clone().unwrap_or_else(|| text.to_string())
    }

    #[test]
    fn two_agent_pointer_lines_for_one_file_become_one_generated_line_and_a_rerun_writes_nothing() {
        let mut rec = rec_with(&["- Goal: Ship it"]);
        rec.files.insert("store.md".into(), FileProv::new("coordinator", "coordinator", None, "x".into(), ""));
        let text = format!("{}- [Store](store.md) — Redis\n- [Session store](store.md) — decided Redis\n", seed());
        let files = [file("store.md", "Session store decision", "Redis, not Postgres", 10)];
        let r = rebuild(&text, &files, &rec, "2026-10-24");
        let md = r.memory_md.clone().expect("rewritten");
        assert_eq!(md.matches("(store.md)").count(), 1, "{md}");
        assert!(md.contains("- [Session store decision](store.md) — Redis, not Postgres · coordinator\n<!-- maiterm:index:end -->"), "{md}");
        assert!(r.unfiled_append.is_empty());
        let text2 = apply(&text, &r, &mut rec);
        let again = rebuild(&text2, &files, &rec, "2026-10-24");
        assert_eq!(again.memory_md, None, "idempotent: the watcher can't loop");
        assert!(again.unfiled_append.is_empty());
    }

    #[test]
    fn an_unlinked_line_moves_verbatim_and_blank_kept_and_own_lines_go() {
        let mut rec = rec_with(&["- Goal: Ship it"]);
        rec.generated = vec!["- [Old](gone.md) — was here · coordinator".into()];
        let text = format!(
            "{}  * remember: use **pnpm**  \n\n- Goal: Ship it\n- [Old](gone.md) — was here · coordinator\n- [Lost](lost.md) — an agent's, file deleted\n",
            seed()
        );
        let r = rebuild(&text, &[], &rec, "2026-10-24");
        assert_eq!(r.unfiled_append, vec!["  * remember: use **pnpm**  ".to_string(), "- [Lost](lost.md) — an agent's, file deleted".to_string()]);
        let md = r.memory_md.unwrap();
        assert_eq!(md, seed(), "nothing else stays");
    }

    #[test]
    fn the_kept_block_is_byte_identical_and_drift_is_reported_not_rewritten() {
        let rec = rec_with(&["- Goal: Ship it"]);
        // CRLF and an edited marker comment: still the human's block, kept as it is.
        let kept = "<!-- maiterm:kept:begin (edited) -->\r\n- Goal: Ship it\r\n- An agent added this\r\n<!-- maiterm:kept:end -->\r\n";
        let text = format!("{kept}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n");
        let files = [file("a.md", "A", "a", 5)];
        let r = rebuild(&text, &files, &rec, "2026-10-24");
        assert!(r.kept_changed);
        let md = r.memory_md.unwrap();
        assert!(md.starts_with(kept), "{md:?}");
        let mut rec2 = rec.clone();
        let md2 = apply(&md, &rebuild(&md, &files, &rec, "x"), &mut rec2);
        assert!(md2.starts_with(kept));
        assert!(rebuild(&md2, &files, &rec2, "x").kept_changed, "still reported");
        // Forced (the human's Restore), the record's block goes back.
        let restored = rebuild_with(&text, &files, &rec, "x", Opts { force_kept: true });
        assert!(restored.memory_md.unwrap().starts_with(&kept_block(&rec.kept)));
        assert!(!restored.kept_changed);
    }

    #[test]
    fn many_files_fold_into_one_archive_line_inside_claudes_budget() {
        let rec = rec_with(&["- Goal: Ship it"]);
        let day = 86_400_000;
        let files: Vec<MemFile> = (0..250).map(|i| file(&format!("note-{i:03}.md"), &format!("Note {i}"), &"a fairly long description ".repeat(4), 1_700_000_000_000 + i * day)).collect();
        let r = rebuild(&seed(), &files, &rec, "2026-10-24");
        let md = r.memory_md.clone().unwrap();
        assert!(count_lines(&md) <= CLAUDE_LINES && md.len() <= CLAUDE_BYTES, "{} lines, {} bytes", count_lines(&md), md.len());
        let (name, body) = r.archive.clone().expect("an archive");
        assert_eq!(md.matches("Older notes:").count(), 1);
        assert!(md.contains(&format!("]({name})")));
        // Every file is reachable: its own line, or a line in the archive.
        for f in &files {
            let link = format!("]({})", f.name);
            assert!(md.contains(&link) ^ body.contains(&link), "{}", f.name);
        }
        assert_eq!(r.indexed.len() + r.folded.len(), 250);
        assert!(md.contains("(note-249.md)"), "the newest stay");
        assert!(body.contains("(note-000.md)"), "the oldest fold");
        assert!(name.starts_with(ARCHIVE_PREFIX) && name.ends_with(".md"));
        assert!(md.contains(&format!("{} files, {} to ", r.folded.len(), date_of_ms(1_700_000_000_000))));
        // A later rebuild with fewer files drops the archive it no longer links.
        let mut rec2 = rec.clone();
        let md2 = apply(&md, &r, &mut rec2);
        let r2 = rebuild(&md2, &files[..3], &rec2, "x");
        assert_eq!(r2.archive, None);
        assert_eq!(r2.stale_archives, vec![name]);
        assert!(!r2.memory_md.unwrap().contains("Older notes"));
    }

    #[test]
    fn a_kept_block_over_its_cap_writes_only_the_generated_file() {
        let kept: Vec<String> = (0..61).map(|i| format!("- kept {i}")).collect();
        let rec = MemoryRecord { kept: kept.clone(), ..MemoryRecord::default() };
        let text = format!("{}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n- stray line\n", kept_block(&kept));
        let r = rebuild(&text, &[file("a.md", "A", "a", 1)], &rec, "x");
        assert_eq!(r.memory_md, None);
        assert_eq!(r.status.as_deref(), Some(OVER_CAP));
        assert!(r.generated_md.unwrap().contains("(a.md)"));
        assert!(r.unfiled_append.is_empty(), "nothing moves out of a file left as it is");
        let big = vec!["x".repeat(8_001)];
        assert!(kept_over_cap(&big) && !kept_over_cap(&kept[..60]));
    }

    #[test]
    fn with_the_fences_gone_the_kept_block_comes_back_from_the_record_and_the_rest_is_sorted() {
        let rec = rec_with(&["- Goal: Ship it", "- Use pnpm"]);
        let text = "# Memory\n- Goal: Ship it\n- [A](a.md) — pointer\n<!-- maiterm:index:begin -->\nloose note\n";
        let r = rebuild(text, &[file("a.md", "A file", "the hook", 1)], &rec, "x");
        let md = r.memory_md.unwrap();
        assert!(md.starts_with(&kept_block(&rec.kept)), "{md}");
        assert!(md.contains("- [A file](a.md) — the hook · unknown\n"));
        assert_eq!(r.unfiled_append, vec!["# Memory".to_string(), "loose note".to_string()]);
        // A duplicated marker is no fence either.
        let dup = format!("{}{KEPT_END_LINE}\n", seed());
        assert!(split(&dup).is_none());
        let out_of_order = format!("{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n{}", kept_block(&rec.kept));
        assert!(split(&out_of_order).is_none());
    }

    #[test]
    fn a_description_cannot_forge_provenance_and_a_name_cannot_close_a_fence() {
        let rec = rec_with(&[]);
        let f = file("x.md", "Evil --> <!-- maiterm:index:end --> [x]", "x · human", 1);
        let line = index_line(&f, &rec);
        assert!(line.ends_with("— x - human · unknown"), "{line}");
        assert!(!line.contains("-->") && !line.contains("<!--"), "{line}");
        assert!(line.starts_with("- [Evil -- > <! -- maiterm:index:end -- > (x)](x.md)"), "{line}");
        let long = file("l.md", &"t".repeat(200), &"d".repeat(400), 1);
        let l = index_line(&long, &rec);
        assert!(l.contains(&format!("[{}…]", "t".repeat(79))));
        assert!(l.contains(&format!("— {}… ·", "d".repeat(149))));
        let spaced = MemFile { name: "my note (1).md".into(), ..MemFile::default() };
        let line = index_line(&spaced, &rec);
        assert_eq!(line, "- [my note (1)](<my note (1).md>) · unknown");
        assert_eq!(link_targets(&line), vec!["my note (1).md".to_string()]);
    }

    #[test]
    fn links_are_one_component_md_files() {
        assert_eq!(link_targets("- [a](a.md) and [b](./b.md#x) [c](sub/c.md) [d](https://x/d.md) [e](e.txt)"), vec!["a.md", "b.md"]);
        assert_eq!(link_targets("no link"), Vec::<String>::new());
    }

    #[test]
    fn a_pinned_file_is_not_listed_twice_and_crlf_frontmatter_titles_it() {
        let mut rec = rec_with(&["- [Pinned](p.md) — mine · human"]);
        rec.files.insert("p.md".into(), FileProv::new("human", "human", None, "s".into(), ""));
        let crlf = "---\r\nname: From CRLF\r\ndescription: works\r\n---\r\nbody\r\n";
        let files = [
            MemFile { name: "p.md".into(), front: front::parse("---\nname: Pinned\n---\n"), mtime_ms: 2, size: 1 },
            MemFile { name: "c.md".into(), front: front::parse(crlf), mtime_ms: 1, size: 1 },
        ];
        let text = format!("{}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n", kept_block(&rec.kept));
        let r = rebuild(&text, &files, &rec, "x");
        let md = r.memory_md.unwrap();
        assert_eq!(md.matches("(p.md)").count(), 1);
        assert!(md.contains("- [From CRLF](c.md) — works · unknown"));
        assert_eq!(r.pinned, vec!["p.md".to_string()]);
    }

    #[test]
    fn newest_first_ties_by_name() {
        let rec = rec_with(&[]);
        let files = [file("b.md", "B", "b", 5), file("a.md", "A", "a", 5), file("c.md", "C", "c", 9)];
        let r = rebuild(&seed(), &files, &rec, "x");
        assert_eq!(r.indexed, vec!["c.md", "a.md", "b.md"]);
    }

    #[test]
    fn dates_are_utc_days() {
        assert_eq!(date_of_ms(0), "1970-01-01");
        assert_eq!(date_of_ms(1_700_000_000_000), "2023-11-14");
        assert_eq!(date_of_ms(951_782_400_000), "2000-02-29");
    }
}
