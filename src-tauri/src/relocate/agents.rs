//! Agent runtimes' per-project state (docs/relocate.md §3). Each runtime keys what it keeps per
//! project by the project's PATH, so a moved folder looks like a brand-new project to it: no
//! resumable sessions, no memory, the trust question again. This carries that state across.
//!
//! Every runtime is best effort and independent: by the time this runs the folder has already
//! moved, so one runtime failing must not stop the others, and is reported rather than raised.
//! All of it assumes no agent in the folder is running (`commands/relocate.rs` suspends them
//! first) — a live Claude appends to the transcript path it opened, and would recreate the old
//! project directory behind us. Agents in OTHER projects keep running, and keep writing to the
//! shared files edited here (`.claude.json`, `history.jsonl`): `update_file` redoes an edit the
//! file changed under.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use super::rebase;
use crate::util::json_file::{update_file, update_json};

/// Where each runtime keeps its state. `real()` in the app; scratch folders in tests.
pub struct Homes {
    /// `~/.claude/projects` — shared by every managed account through a symlink.
    pub claude_projects: PathBuf,
    /// `~/.claude.json` and each managed account's own copy: per-project trust, MCP servers.
    pub claude_json: Vec<PathBuf>,
    /// `~/.claude/history.jsonl` and each account's own: up-arrow history, scoped per project.
    pub claude_history: Vec<PathBuf>,
    pub codex: PathBuf,
    pub gemini: PathBuf,
}

impl Homes {
    pub fn real() -> Option<Self> {
        let home = dirs::home_dir()?;
        let mut claude_json = vec![home.join(".claude.json")];
        let mut claude_history = vec![home.join(".claude/history.jsonl")];
        // Account roots hold their OWN `.claude.json` and `history.jsonl` (accounts/mod.rs: the
        // rest is symlinked to ~/.claude, which is already covered).
        if let Some(dir) = crate::accounts::accounts_dir().map(|d| d.join("claude")) {
            for e in fs::read_dir(dir).into_iter().flatten().flatten() {
                for (name, list) in [(".claude.json", &mut claude_json), ("history.jsonl", &mut claude_history)] {
                    let p = e.path().join(name);
                    if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file()) {
                        list.push(p);
                    }
                }
            }
        }
        let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        Some(Self { claude_projects: home.join(".claude/projects"), claude_json, claude_history, codex, gemini: home.join(".gemini") })
    }
}

/// What a move would carry, counted before the human confirms.
#[derive(Debug, Default, Clone, Serialize)]
pub struct AgentPlan {
    pub claude_sessions: usize,
    pub claude_memory: bool,
    pub claude_trust: bool,
    pub codex_sessions: usize,
    pub codex_trust: bool,
    pub gemini_projects: usize,
    pub agy_sessions: usize,
    pub agy_trust: bool,
}

/// What a move did, in sentences for the result toast and the log.
#[derive(Debug, Default, Clone, Serialize)]
pub struct AgentReport {
    pub done: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn plan(old: &Path, new: &Path) -> AgentPlan {
    Homes::real().map(|h| plan_in(&h, old, new)).unwrap_or_default()
}

/// `known` are paths under `old` saved state points at — Gemini's older layout names a project
/// directory by a one-way hash of its path, so only paths we already know can be found.
pub fn apply(old: &Path, new: &Path, known: &[String]) -> AgentReport {
    match Homes::real() {
        Some(h) => apply_in(&h, old, new, known),
        None => AgentReport { warnings: vec!["No home folder — agent state was not moved.".into()], ..Default::default() },
    }
}

pub fn plan_in(h: &Homes, old: &Path, new: &Path) -> AgentPlan {
    let c = claude_plan(&h.claude_projects, old, new);
    let agy_dir = h.gemini.join("antigravity-cli");
    AgentPlan {
        claude_sessions: c.sessions.len(),
        claude_memory: c.dirs.iter().any(|(from, _)| from.join("memory").is_dir()),
        claude_trust: h.claude_json.iter().any(|f| read_json(f).is_some_and(|v| keys_under(&v["projects"], old, new) > 0)),
        codex_sessions: codex_threads(&h.codex, old, new).len(),
        codex_trust: fs::read_to_string(h.codex.join("config.toml")).is_ok_and(|t| codex_trust_keys(&t, old, new) > 0),
        gemini_projects: read_json(&h.gemini.join("projects.json")).map_or(0, |v| keys_under(&v["projects"], old, new)),
        agy_sessions: agy_session_ids(&agy_dir, old, new).len(),
        agy_trust: read_json(&agy_dir.join("settings.json")).is_some_and(|v| agy_trusted_count(&v, old, new) > 0),
    }
}

pub fn apply_in(h: &Homes, old: &Path, new: &Path, known: &[String]) -> AgentReport {
    let mut r = AgentReport::default();
    claude(h, old, new, &mut r);
    codex(h, old, new, &mut r);
    gemini(h, old, new, known, &mut r);
    antigravity(h, old, new, &mut r);
    r
}

// ── shared ──────────────────────────────────────────────────────────────────────────────────

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

/// A JSONL file edited line by line. Only lines that mention `needle` are parsed; the rest are
/// copied byte for byte. `edit` returns whether it changed the line.
fn edit_jsonl(bytes: &[u8], needle: &str, edit: &mut impl FnMut(&mut Value) -> bool) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut changed = false;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let body_len = line.len() - line.iter().rev().take_while(|b| **b == b'\n' || **b == b'\r').count();
        let body = &line[..body_len];
        let n = needle.as_bytes();
        let hit = std::str::from_utf8(body).is_ok() && (n.is_empty() || body.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n)));
        if hit {
            if let Ok(mut v) = serde_json::from_slice::<Value>(body) {
                if edit(&mut v) {
                    out.extend(serde_json::to_vec(&v).ok()?);
                    out.extend(&line[body_len..]);
                    changed = true;
                    continue;
                }
            }
        }
        out.extend(line);
    }
    changed.then_some(out)
}

/// The prefilter is the old folder's last NAME, matched ignoring ASCII case: the whole path is
/// not what the raw line holds on Windows (JSON escapes `C:\x` as `C:\\x`, and either separator
/// may be the one recorded), nor on a volume that folds case. `edit` decides with `rebase`.
fn update_jsonl(p: &Path, old: &Path, mut edit: impl FnMut(&mut Value) -> bool) -> std::io::Result<bool> {
    let needle = old.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    update_file(p, |b| edit_jsonl(b, &needle, &mut edit))
}

fn keys_under(map: &Value, old: &Path, new: &Path) -> usize {
    map.as_object().map_or(0, |m| m.keys().filter(|k| rebase(k, old, new).is_some()).count())
}

/// Re-key every entry of a path-keyed object. An entry already under the new key wins field by
/// field — it is the newer one — and only fills in what it lacks from the old.
fn rekey(map: &mut Map<String, Value>, old: &Path, new: &Path) -> usize {
    let moving: Vec<(String, String)> = map
        .keys()
        .filter_map(|k| rebase(k, old, new).filter(|n| n != k).map(|n| (k.clone(), n)))
        .collect();
    for (from, to) in &moving {
        let v = map.remove(from).unwrap();
        match (map.get_mut(to), v) {
            (Some(Value::Object(dst)), Value::Object(src)) => {
                for (k, v) in src {
                    dst.entry(k).or_insert(v);
                }
            }
            (Some(_), _) => {}
            (None, v) => {
                map.insert(to.clone(), v);
            }
        }
    }
    moving.len()
}

// ── Claude ──────────────────────────────────────────────────────────────────────────────────

/// Claude's project directory name for a path (its `mP`): every UTF-16 unit outside
/// `[A-Za-z0-9]` becomes `-`; past 200 characters, cut and suffixed with a base-36 Java string
/// hash of the full path.
pub fn claude_slug(path: &str) -> String {
    let mut s = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                s.push('-');
            }
        }
    }
    if s.len() <= 200 {
        return s;
    }
    let mut h: i32 = 0;
    for u in path.encode_utf16() {
        h = h.wrapping_shl(5).wrapping_sub(h).wrapping_add(u as i32);
    }
    let mut n = (h as i64).unsigned_abs();
    let mut digits = vec![];
    loop {
        digits.push(std::char::from_digit((n % 36) as u32, 36).unwrap());
        n /= 36;
        if n == 0 {
            break;
        }
    }
    format!("{}-{}", &s[..200], digits.iter().rev().collect::<String>())
}

/// The project folder a session was started in, from the first transcript line that records
/// one. Read from the head only: transcripts run to hundreds of MB.
fn first_cwd(jsonl: &Path) -> Option<String> {
    let f = fs::File::open(jsonl).ok()?;
    for line in BufReader::new(f).lines().take(50).map_while(Result::ok) {
        if let Some(c) = serde_json::from_str::<Value>(&line).ok().and_then(|v| v["cwd"].as_str().map(String::from)) {
            return Some(c);
        }
    }
    None
}

/// How much of a transcript's end Claude reads for its session metadata (2.1.289: `ZS`, 64 KB,
/// the buffer its head/tail reader `avt` fills). Claude re-appends that metadata — the
/// `relocated` record included — at the end of every run, so the current one is always there.
const CLAUDE_TAIL: u64 = 64 * 1024;

/// The folder a session belongs to NOW, read the way Claude reads it (`ne`: the last
/// `relocated` record in the 64 KB tail, else the first `cwd` in the head). A session moved
/// before — by this or by Claude itself — keeps its original folder on line one. Only the tail:
/// transcripts run to hundreds of MB, this runs for every session in the preview, and agreeing
/// with Claude's own lookup is the point.
fn session_cwd(jsonl: &Path) -> Option<String> {
    tail_relocated(jsonl).or_else(|| first_cwd(jsonl))
}

fn tail_relocated(jsonl: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(jsonl).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(CLAUDE_TAIL))).ok()?;
    let mut buf = Vec::new();
    f.take(CLAUDE_TAIL).read_to_end(&mut buf).ok()?;
    // From the end, like Claude's `q`: the first (= last) line that is a relocated record. A
    // line cut by the window's start fails to parse and is skipped, as there.
    buf.split(|b| *b == b'\n').rev().find_map(|line| {
        let s = std::str::from_utf8(line).ok()?;
        if !(s.contains("\"relocatedCwd\":") && s.contains("\"type\":\"relocated\"")) {
            return None;
        }
        let v: Value = serde_json::from_str(s).ok()?;
        (v["type"] == "relocated").then(|| v["relocatedCwd"].as_str().map(String::from)).flatten()
    })
}

fn sessions_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    v.sort();
    v
}

struct ClaudeSession {
    jsonl: PathBuf,
    /// The folder it belongs to, after the move — its new project directory is this's slug.
    new_cwd: String,
}

#[derive(Default)]
struct ClaudePlan {
    /// Every session started under `old`, decided ONE BY ONE: the slug is lossy (`/u/p/web`, a
    /// subfolder, and `/u/p-web`, a sibling project, share `-u-p-web`), so one directory can
    /// hold sessions of two projects, and only the ones that are ours move.
    sessions: Vec<ClaudeSession>,
    /// Directories whose remaining entries (`memory/`, indexes) move too, and where to: only
    /// one holding nothing of another project's — anything else can't be told apart.
    dirs: Vec<(PathBuf, PathBuf)>,
    /// Memory left behind because its directory is shared with another project.
    stuck_memory: Vec<PathBuf>,
}

fn claude_plan(projects: &Path, old: &Path, new: &Path) -> ClaudePlan {
    let old_slug = claude_slug(&old.to_string_lossy());
    let fold = super::case_insensitive(old);
    let norm = |s: &str| if fold { s.to_lowercase() } else { s.to_string() };
    let mut plan = ClaudePlan::default();
    for e in fs::read_dir(projects).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let (n, o) = (norm(&name), norm(&old_slug));
        if !(n == o || n.starts_with(&format!("{o}-"))) || !e.path().is_dir() {
            continue;
        }
        let (mut ours, mut foreign) = (0, 0);
        let mut first_new: Option<String> = None;
        for s in sessions_in(&e.path()) {
            match session_cwd(&s).map(|c| rebase(&c, old, new)) {
                Some(Some(new_cwd)) => {
                    ours += 1;
                    first_new.get_or_insert_with(|| new_cwd.clone());
                    plan.sessions.push(ClaudeSession { jsonl: s, new_cwd });
                }
                // Started in a folder outside `old`: the only real evidence of another project.
                Some(None) => foreign += 1,
                // Records no folder at all — Claude leaves stubs holding only
                // `file-history-snapshot` lines (17 in this repo's own directory). Not evidence
                // of anything: it stays with its directory, and moves with it if that moves.
                None => {}
            }
        }
        let target = if n == o { Some(new.to_string_lossy().to_string()) } else { first_new };
        match target {
            Some(t) if foreign == 0 => plan.dirs.push((e.path(), projects.join(claude_slug(&t)))),
            _ if e.path().join("memory").is_dir() && ours > 0 => plan.stuck_memory.push(e.path().join("memory")),
            _ => {}
        }
    }
    plan
}

/// Move `src`'s entries into `dst` one by one; a name already in `dst` stays behind (memory
/// files merge the same way, one level down). Returns what stayed behind.
fn merge_dir(src: &Path, dst: &Path) -> std::io::Result<Vec<PathBuf>> {
    fs::create_dir_all(dst)?;
    let mut left = vec![];
    for e in fs::read_dir(src)?.flatten() {
        let to = dst.join(e.file_name());
        if !to.exists() {
            fs::rename(e.path(), &to)?;
        } else if e.file_name() == "memory" && e.path().is_dir() && to.is_dir() {
            left.extend(merge_dir(&e.path(), &to)?);
        } else {
            left.push(e.path());
        }
    }
    if left.is_empty() {
        let _ = fs::remove_dir(src);
    }
    Ok(left)
}

/// Claude's own record for a session whose folder changed (`relocateSessionTranscript`): the
/// last one wins over every `cwd` before it, so history needs no rewriting.
fn append_relocated(jsonl: &Path, new_cwd: &str) -> std::io::Result<()> {
    let sid = jsonl.file_stem().unwrap_or_default().to_string_lossy().to_string();
    // A transcript whose last line has no newline would glue ours onto it.
    let needs_nl = {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = fs::File::open(jsonl)?;
        let len = f.metadata()?.len();
        let mut last = [0u8; 1];
        len > 0 && f.seek(SeekFrom::End(-1)).is_ok() && f.read_exact(&mut last).is_ok() && last[0] != b'\n'
    };
    let mut f = fs::OpenOptions::new().append(true).open(jsonl)?;
    let rec = serde_json::json!({ "type": "relocated", "sessionId": sid, "relocatedCwd": new_cwd });
    if needs_nl {
        f.write_all(b"\n")?;
    }
    writeln!(f, "{rec}")
}

/// Symlinks inside a moved session folder that point into its old project directory.
fn repoint_links(root: &Path, from: &Path, to: &Path) {
    for e in fs::read_dir(root).into_iter().flatten().flatten() {
        let p = e.path();
        let Ok(meta) = fs::symlink_metadata(&p) else { continue };
        if meta.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(&p) {
                if let Ok(rest) = target.strip_prefix(from) {
                    relink(&p, &to.join(rest));
                }
            }
        } else if meta.is_dir() {
            repoint_links(&p, from, to);
        }
    }
}

#[cfg(unix)]
fn relink(link: &Path, target: &Path) {
    let _ = fs::remove_file(link);
    let _ = std::os::unix::fs::symlink(target, link);
}

/// Making a link on Windows can need a privilege the user lacks, so the new one is made beside
/// the old and swapped in only once it exists — a failure leaves the old link, not none.
#[cfg(windows)]
fn relink(link: &Path, target: &Path) {
    use std::os::windows::fs::{symlink_dir, symlink_file, FileTypeExt};
    let mut name = link.file_name().unwrap_or_default().to_os_string();
    name.push(".maiterm-relink");
    let tmp = link.with_file_name(name);
    // The old link's own kind, not its target's: the target may not have been moved in yet.
    let dir = fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink_dir());
    let made = if dir { symlink_dir(target, &tmp) } else { symlink_file(target, &tmp) };
    if made.is_ok() {
        // A directory link is removed as a directory on Windows.
        let _ = fs::remove_file(link).or_else(|_| fs::remove_dir(link));
        if fs::rename(&tmp, link).is_err() {
            let _ = fs::remove_file(&tmp).or_else(|_| fs::remove_dir(&tmp));
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn relink(_: &Path, _: &Path) {}

fn claude(h: &Homes, old: &Path, new: &Path, r: &mut AgentReport) {
    let plan = claude_plan(&h.claude_projects, old, new);
    let mut moved = 0;
    for s in &plan.sessions {
        let from_dir = s.jsonl.parent().unwrap_or(Path::new("")).to_path_buf();
        let to_dir = h.claude_projects.join(claude_slug(&s.new_cwd));
        let name = s.jsonl.file_name().unwrap_or_default();
        let to = to_dir.join(name);
        if to.exists() {
            r.warnings.push(format!("Claude: session {} is already in the new project — left in {}.", name.to_string_lossy(), from_dir.display()));
            continue;
        }
        let res = fs::create_dir_all(&to_dir).and_then(|_| fs::rename(&s.jsonl, &to));
        if let Err(e) = res {
            r.warnings.push(format!("Claude: couldn't move session {}: {e}", s.jsonl.display()));
            continue;
        }
        // Its own folder (subagents, tool results) goes with it.
        let stem = s.jsonl.file_stem().unwrap_or_default();
        let (sub_from, sub_to) = (from_dir.join(stem), to_dir.join(stem));
        if sub_from.is_dir() && !sub_to.exists() {
            if let Err(e) = fs::rename(&sub_from, &sub_to) {
                r.warnings.push(format!("Claude: couldn't move {}: {e}", sub_from.display()));
            } else {
                repoint_links(&sub_to, &from_dir, &to_dir);
            }
        }
        match append_relocated(&to, &s.new_cwd) {
            Ok(()) => moved += 1,
            Err(e) => r.warnings.push(format!("Claude: couldn't mark {} as moved: {e}", to.display())),
        }
    }
    let mut memory = false;
    for (from, to) in &plan.dirs {
        if !from.exists() {
            continue; // emptied by the session moves and nothing else in it
        }
        memory |= from.join("memory").is_dir();
        match merge_dir(from, to) {
            Ok(left) => {
                for p in left {
                    r.warnings.push(format!(
                        "Claude: {} was already in the new project and was left in {} — merge it by hand.",
                        p.file_name().unwrap_or_default().to_string_lossy(),
                        from.display()
                    ));
                }
            }
            Err(e) => r.warnings.push(format!("Claude: couldn't move {}: {e}", from.display())),
        }
        // Emptied of everything ours: don't leave a husk named after the old path.
        let _ = fs::remove_dir(from);
    }
    for m in &plan.stuck_memory {
        r.warnings.push(format!(
            "Claude: project memory in {} wasn't moved — that folder also holds another project's sessions. Move it by hand if it's this project's.",
            m.display()
        ));
    }
    if moved > 0 || memory {
        r.done.push(format!("Claude: {moved} session(s){} moved", if memory { " and project memory" } else { "" }));
    }

    let mut trust_files = 0;
    for f in &h.claude_json {
        let res = update_json(f, |v| {
            let mut n = v.get_mut("projects").and_then(Value::as_object_mut).map_or(0, |m| rekey(m, old, new));
            if let Some(repos) = v.get_mut("githubRepoPaths").and_then(Value::as_object_mut) {
                for list in repos.values_mut().filter_map(Value::as_array_mut) {
                    for p in list.iter_mut() {
                        if let Some(rb) = p.as_str().and_then(|s| rebase(s, old, new)) {
                            if p.as_str() != Some(rb.as_str()) {
                                *p = Value::String(rb);
                                n += 1;
                            }
                        }
                    }
                    let mut seen = vec![];
                    list.retain(|p| {
                        let keep = !seen.contains(p);
                        seen.push(p.clone());
                        keep
                    });
                }
            }
            n > 0
        });
        match res {
            Ok(true) => trust_files += 1,
            Ok(false) => {}
            Err(e) => r.warnings.push(format!("Claude: couldn't update {}: {e}", f.display())),
        }
    }
    if trust_files > 0 {
        r.done.push(format!("Claude: project settings and trust moved in {trust_files} config file(s)"));
    }

    for f in &h.claude_history {
        let res = update_jsonl(f, old, |v| {
            let p = v.get("project").and_then(Value::as_str).and_then(|s| rebase(s, old, new));
            p.map(|p| v["project"] = Value::String(p)).is_some()
        });
        if let Err(e) = res {
            r.warnings.push(format!("Claude: couldn't update prompt history {}: {e}", f.display()));
        }
    }
}

// ── Codex ───────────────────────────────────────────────────────────────────────────────────

fn codex_trust_keys(toml: &str, old: &Path, new: &Path) -> usize {
    let Ok(doc) = toml.parse::<toml_edit::DocumentMut>() else { return 0 };
    doc.get("projects").and_then(|p| p.as_table_like()).map_or(0, |t| t.iter().filter(|(k, _)| rebase(k, old, new).is_some()).count())
}

/// (state db, thread id, cwd, rollout path) of every Codex thread started under `old`.
fn codex_threads(codex: &Path, old: &Path, new: &Path) -> Vec<(PathBuf, String, String, String)> {
    let mut out = vec![];
    for e in fs::read_dir(codex).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("state_") && name.ends_with(".sqlite")) {
            continue;
        }
        let Ok(db) = rusqlite::Connection::open_with_flags(e.path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { continue };
        let Ok(mut st) = db.prepare("SELECT id, cwd, rollout_path FROM threads") else { continue };
        let rows = st.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)));
        for (id, cwd, rollout) in rows.into_iter().flatten().flatten() {
            if rebase(&cwd, old, new).is_some() {
                out.push((e.path(), id, cwd, rollout.unwrap_or_default()));
            }
        }
    }
    out
}

fn codex(h: &Homes, old: &Path, new: &Path, r: &mut AgentReport) {
    // Trust: `[projects."<path>"]`, edited in place so the rest of the file keeps its layout.
    let cfg = h.codex.join("config.toml");
    let res = update_file(&cfg, |b| {
        let mut doc = std::str::from_utf8(b).ok()?.parse::<toml_edit::DocumentMut>().ok()?;
        let t = doc.get_mut("projects").and_then(|p| p.as_table_like_mut())?;
        let moving: Vec<(String, String)> = t
            .iter()
            .filter_map(|(k, _)| rebase(k, old, new).filter(|nk| nk != k).map(|nk| (k.to_string(), nk)))
            .collect();
        if moving.is_empty() {
            return None;
        }
        for (from, to) in moving {
            if let Some(item) = t.remove(&from) {
                if t.get(&to).is_none() {
                    t.insert(&to, item);
                }
            }
        }
        Some(doc.to_string().into_bytes())
    });
    match res {
        Ok(true) => r.done.push("Codex: project trust moved".into()),
        Ok(false) => {}
        Err(e) => r.warnings.push(format!("Codex: couldn't update {}: {e}", cfg.display())),
    }

    // Sessions: the thread index's cwd, and the cwd each rollout records — `codex resume` asks
    // which folder to use when the recorded one is gone, which would stall an auto-resume.
    let mut moved = 0;
    for (db_path, id, cwd, rollout) in codex_threads(&h.codex, old, new) {
        let Some(new_cwd) = rebase(&cwd, old, new) else { continue };
        let db = rusqlite::Connection::open(&db_path).and_then(|db| {
            db.busy_timeout(std::time::Duration::from_secs(5))?;
            db.execute("UPDATE threads SET cwd = ?1 WHERE id = ?2", rusqlite::params![new_cwd, id])
        });
        if let Err(e) = db {
            r.warnings.push(format!("Codex: couldn't update session {id}: {e}"));
            continue;
        }
        if !rollout.is_empty() {
            let res = update_jsonl(Path::new(&rollout), old, |v| {
                let kind = v["type"].as_str().unwrap_or_default();
                if kind != "session_meta" && kind != "turn_context" {
                    return false;
                }
                let n = v["payload"]["cwd"].as_str().and_then(|c| rebase(c, old, new));
                n.map(|n| v["payload"]["cwd"] = Value::String(n)).is_some()
            });
            if let Err(e) = res {
                r.warnings.push(format!("Codex: couldn't update the transcript of session {id}: {e}"));
            }
        }
        moved += 1;
    }
    if moved > 0 {
        r.done.push(format!("Codex: {moved} session(s) moved"));
    }
}

// ── Gemini ──────────────────────────────────────────────────────────────────────────────────

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn gemini(h: &Homes, old: &Path, new: &Path, known: &[String], r: &mut AgentReport) {
    let g = &h.gemini;
    let mut moved = 0;
    // Current layout: `projects.json` maps a path to a slug, and the slug's folders carry a
    // `.project_root` naming the path back — both must agree, or Gemini claims a fresh slug and
    // the history looks lost.
    let reg = g.join("projects.json");
    let mut slugs: Vec<String> = vec![];
    let res = update_json(&reg, |v| {
        let Some(m) = v.get_mut("projects").and_then(Value::as_object_mut) else { return false };
        slugs = m.iter().filter(|(k, _)| rebase(k, old, new).is_some()).filter_map(|(_, s)| s.as_str().map(String::from)).collect();
        rekey(m, old, new) > 0
    });
    match res {
        Ok(true) => moved += slugs.len(),
        Ok(false) => slugs.clear(),
        Err(e) => {
            slugs.clear();
            r.warnings.push(format!("Gemini: couldn't update {}: {e}", reg.display()));
        }
    }
    for slug in &slugs {
        for base in ["tmp", "history"] {
            let marker = g.join(base).join(slug).join(".project_root");
            let res = update_file(&marker, |b| {
                let n = rebase(std::str::from_utf8(b).ok()?.trim(), old, new)?;
                Some(n.into_bytes())
            });
            if let Err(e) = res {
                r.warnings.push(format!("Gemini: couldn't update {}: {e}", marker.display()));
            }
        }
    }
    // Older layout: `tmp/<sha256(path)>`. One-way, so only paths we know can be found.
    let mut paths: Vec<String> = known.to_vec();
    paths.push(old.to_string_lossy().to_string());
    paths.sort();
    paths.dedup();
    for p in &paths {
        let abs = super::expand_home(p).to_string_lossy().to_string();
        let Some(np) = rebase(&abs, old, new) else { continue };
        for base in ["tmp", "history"] {
            let (from, to) = (g.join(base).join(sha256_hex(&abs)), g.join(base).join(sha256_hex(&np)));
            if from.is_dir() && !to.exists() {
                match fs::rename(&from, &to) {
                    Ok(()) if base == "tmp" => moved += 1,
                    Ok(()) => {}
                    Err(e) => r.warnings.push(format!("Gemini: couldn't move {}: {e}", from.display())),
                }
            }
        }
    }
    let trust = g.join("trustedFolders.json");
    if let Err(e) = update_json(&trust, |v| v.as_object_mut().map_or(0, |m| rekey(m, old, new)) > 0) {
        r.warnings.push(format!("Gemini: couldn't update {}: {e}", trust.display()));
    }
    if moved > 0 {
        r.done.push(format!("Gemini: {moved} project(s) moved"));
    }
}

// ── Antigravity (agy) ───────────────────────────────────────────────────────────────────────

/// Rebase a `file://` workspace URI from `old` onto `new` (percent-decoding the path, matching
/// component boundaries via `rebase`, and re-encoding as a `file://` URI). Non-`file://` URIs,
/// remote `file://host/...` URIs, URIs with query/fragment, and paths outside `old` return `None`.
fn rebase_file_uri(uri: &str, old: &Path, new: &Path) -> Option<String> {
    let u = reqwest::Url::parse(uri).ok()?;
    if u.scheme() != "file" || u.query().is_some() || u.fragment().is_some() {
        return None;
    }
    let path = u.to_file_path().ok()?;
    let rebased = rebase(path.to_str()?, old, new)?;
    reqwest::Url::from_file_path(&rebased).ok().map(String::from)
}

fn dedup_values(list: &mut Vec<Value>) {
    let mut seen = Vec::with_capacity(list.len());
    list.retain(|v| {
        let keep = !seen.contains(v);
        if keep {
            seen.push(v.clone());
        }
        keep
    });
}

fn rebase_path_array(list: &mut Vec<Value>, old: &Path, new: &Path) -> bool {
    let mut changed = false;
    for item in list.iter_mut() {
        if let Some(orig) = item.as_str() {
            if let Some(rb) = rebase(orig, old, new) {
                if rb != orig {
                    *item = Value::String(rb);
                    changed = true;
                }
            }
        }
    }
    if changed {
        dedup_values(list);
    }
    changed
}

fn rebase_uri_array(list: &mut Vec<Value>, old: &Path, new: &Path) -> bool {
    let mut changed = false;
    for item in list.iter_mut() {
        if let Some(orig) = item.as_str() {
            if let Some(rb) = rebase_file_uri(orig, old, new) {
                if rb != orig {
                    *item = Value::String(rb);
                    changed = true;
                }
            }
        }
    }
    if changed {
        dedup_values(list);
    }
    changed
}

fn rebase_workspace_uris_json(raw: &str, old: &Path, new: &Path) -> Option<String> {
    let mut v: Value = serde_json::from_str(raw).ok()?;
    let list = v.as_array_mut()?;
    if !rebase_uri_array(list, old, new) {
        return None;
    }
    serde_json::to_string(&v).ok()
}

fn agy_trusted_count(v: &Value, old: &Path, new: &Path) -> usize {
    v.get("trustedWorkspaces")
        .and_then(Value::as_array)
        .map_or(0, |list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|s| rebase(s, old, new).is_some_and(|rb| rb != *s))
                .count()
        })
}

fn is_missing_table_err(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains("no such table"))
}

/// Conversation IDs in `conversation_summaries.db` whose `workspace_uris` array has an entry under `old`.
fn agy_summary_ids(db_path: &Path, old: &Path, new: &Path) -> rusqlite::Result<Vec<String>> {
    if !db_path.is_file() {
        return Ok(vec![]);
    }
    let db = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut st = match db.prepare("SELECT conversation_id, workspace_uris FROM conversation_summaries") {
        Ok(st) => st,
        Err(e) if is_missing_table_err(&e) => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let rows = st.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut ids = vec![];
    for row in rows {
        let (id, raw) = row?;
        if rebase_workspace_uris_json(&raw, old, new).is_some() {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Unique Antigravity conversation IDs associated with `old` across `conversation_summaries.db`,
/// `cache/conversation_metadata.json`, and `cache/last_conversations.json`.
fn agy_session_ids(agy: &Path, old: &Path, new: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(db_ids) = agy_summary_ids(&agy.join("conversation_summaries.db"), old, new) {
        ids.extend(db_ids);
    }
    if let Some(meta) = read_json(&agy.join("cache/conversation_metadata.json")) {
        if let Some(convs) = meta.get("conversations").and_then(Value::as_object) {
            for (id, entry) in convs {
                let hit = entry
                    .get("summary")
                    .and_then(|s| s.get("WorkspaceURIs"))
                    .and_then(Value::as_array)
                    .is_some_and(|uris| {
                        uris.iter()
                            .filter_map(Value::as_str)
                            .any(|u| rebase_file_uri(u, old, new).is_some_and(|rb| rb != u))
                    });
                if hit {
                    ids.insert(id.clone());
                }
            }
        }
    }
    if let Some(last) = read_json(&agy.join("cache/last_conversations.json")) {
        if let Some(m) = last.as_object() {
            for (k, v) in m {
                if rebase(k, old, new).is_some_and(|rb| rb != *k) {
                    let id = v.as_str().filter(|s| !s.is_empty()).unwrap_or(k);
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}

fn update_agy_summary_db(db_path: &Path, old: &Path, new: &Path) -> rusqlite::Result<Vec<String>> {
    let mut db = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let updates: Vec<(String, String)> = {
        let mut st = match tx.prepare("SELECT conversation_id, workspace_uris FROM conversation_summaries") {
            Ok(st) => st,
            Err(e) if is_missing_table_err(&e) => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let rows = st.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut out = vec![];
        for row in rows {
            let (id, raw) = row?;
            if let Some(updated) = rebase_workspace_uris_json(&raw, old, new) {
                out.push((id, updated));
            }
        }
        out
    };
    if !updates.is_empty() {
        let mut upd = tx.prepare("UPDATE conversation_summaries SET workspace_uris = ?1 WHERE conversation_id = ?2")?;
        for (id, updated) in &updates {
            upd.execute(rusqlite::params![updated, id])?;
        }
    }
    tx.commit()?;
    Ok(updates.into_iter().map(|(id, _)| id).collect())
}

fn antigravity(h: &Homes, old: &Path, new: &Path, r: &mut AgentReport) {
    let agy = h.gemini.join("antigravity-cli");
    if !agy.is_dir() {
        return;
    }

    // 1. Folder trust: `settings.json` `trustedWorkspaces: string[]`.
    let settings = agy.join("settings.json");
    let res = update_json(&settings, |v| {
        v.get_mut("trustedWorkspaces")
            .and_then(Value::as_array_mut)
            .is_some_and(|list| rebase_path_array(list, old, new))
    });
    match res {
        Ok(true) => r.done.push("Antigravity: folder trust moved".into()),
        Ok(false) => {}
        Err(e) => r.warnings.push(format!("Antigravity: couldn't update {}: {e}", settings.display())),
    }

    // 2. Prompt history: `history.jsonl` `"workspace"` field.
    let hist = agy.join("history.jsonl");
    if let Err(e) = update_jsonl(&hist, old, |v| {
        let Some(orig) = v.get("workspace").and_then(Value::as_str) else { return false };
        let Some(rb) = rebase(orig, old, new).filter(|n| n != orig) else { return false };
        v["workspace"] = Value::String(rb);
        true
    }) {
        r.warnings.push(format!("Antigravity: couldn't update prompt history {}: {e}", hist.display()));
    }

    let mut moved_ids: HashSet<String> = HashSet::new();

    // 3. `cache/last_conversations.json`: `{ "<abs_path>": "<conversation_id>" }` (used by `agy --continue`).
    let last_conv = agy.join("cache/last_conversations.json");
    let mut lc_ids: Vec<String> = vec![];
    let res = update_json(&last_conv, |v| {
        lc_ids.clear();
        let Some(m) = v.as_object_mut() else { return false };
        for (k, val) in m.iter() {
            if rebase(k, old, new).is_some_and(|rb| rb != *k) {
                let id = val.as_str().filter(|s| !s.is_empty()).unwrap_or(k);
                lc_ids.push(id.to_string());
            }
        }
        rekey(m, old, new) > 0
    });
    match res {
        Ok(true) => moved_ids.extend(lc_ids),
        Ok(false) => {}
        Err(e) => r.warnings.push(format!("Antigravity: couldn't update {}: {e}", last_conv.display())),
    }

    // 4. `cache/conversation_metadata.json`: `conversations.<id>.summary.WorkspaceURIs`.
    let meta = agy.join("cache/conversation_metadata.json");
    let mut meta_ids: Vec<String> = vec![];
    let res = update_json(&meta, |v| {
        meta_ids.clear();
        let Some(convs) = v.get_mut("conversations").and_then(Value::as_object_mut) else { return false };
        let mut changed = false;
        for (id, entry) in convs.iter_mut() {
            if let Some(uris) = entry
                .get_mut("summary")
                .and_then(|s| s.get_mut("WorkspaceURIs"))
                .and_then(Value::as_array_mut)
            {
                if rebase_uri_array(uris, old, new) {
                    meta_ids.push(id.clone());
                    changed = true;
                }
            }
        }
        changed
    });
    match res {
        Ok(true) => moved_ids.extend(meta_ids),
        Ok(false) => {}
        Err(e) => r.warnings.push(format!("Antigravity: couldn't update {}: {e}", meta.display())),
    }

    // 5. `conversation_summaries.db`: `conversation_summaries.workspace_uris` JSON array column.
    //    Opaque protobuf blobs (`raw_summary`, `conversations/<id>.db`'s `trajectory_metadata_blob`)
    //    and the derived FTS5 index (`conversations_fts`) are intentionally left untouched.
    let db_path = agy.join("conversation_summaries.db");
    if db_path.is_file() {
        match agy_summary_ids(&db_path, old, new) {
            Ok(ids) if ids.is_empty() => {}
            Ok(_) => match update_agy_summary_db(&db_path, old, new) {
                Ok(updated) => moved_ids.extend(updated),
                Err(e) => r.warnings.push(format!("Antigravity: couldn't update {}: {e}", db_path.display())),
            },
            Err(e) => r.warnings.push(format!("Antigravity: couldn't read {}: {e}", db_path.display())),
        }
    }

    if !moved_ids.is_empty() {
        r.done.push(format!("Antigravity: {} session(s) moved", moved_ids.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_slug_matches_the_real_directories() {
        assert_eq!(claude_slug("/Users/dprusak/DATA/IDE/aiTerm"), "-Users-dprusak-DATA-IDE-aiTerm");
        assert_eq!(claude_slug("/Users/d/EnagicMobi.f7svelte"), "-Users-d-EnagicMobi-f7svelte");
        assert_eq!(claude_slug("/a/ValHeim Server Mods/_BepInEx"), "-a-ValHeim-Server-Mods--BepInEx");
        assert_eq!(claude_slug("/a/com~apple"), "-a-com-apple");
        let long = format!("/{}", "x".repeat(250));
        let s = claude_slug(&long);
        assert!(s.starts_with(&format!("-{}", "x".repeat(199))));
        assert_eq!(s.as_bytes()[200], b'-');
    }

    struct Scratch {
        base: PathBuf,
        h: Homes,
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
    fn scratch() -> Scratch {
        let base = std::env::temp_dir().join(format!("maiterm-agents-{}", uuid::Uuid::new_v4()));
        let h = Homes {
            claude_projects: base.join("claude/projects"),
            claude_json: vec![base.join("claude.json"), base.join("account/.claude.json")],
            claude_history: vec![base.join("claude/history.jsonl")],
            codex: base.join("codex"),
            gemini: base.join("gemini"),
        };
        for d in [&h.claude_projects, &h.codex, &h.gemini, &base.join("account")] {
            fs::create_dir_all(d).unwrap();
        }
        Scratch { base, h }
    }

    // Fixtures written as Unix paths (`/u/…` isn't absolute on Windows) are `cfg(unix)`; the
    // Windows forms have their own test below.
    #[cfg(unix)]
    #[test]
    fn claude_sessions_memory_trust_and_history_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new("/u/IDE/aiTerm"), Path::new("/u/IDE/maiterm"));
        let p = &s.h.claude_projects;
        // The project's own dir, a subfolder's, and a SIBLING sharing the prefix.
        let root = p.join("-u-IDE-aiTerm");
        fs::create_dir_all(root.join("memory")).unwrap();
        fs::write(root.join("memory/MEMORY.md"), "m").unwrap();
        fs::write(root.join("s1.jsonl"), "{\"type\":\"user\",\"cwd\":\"/u/IDE/aiTerm\"}\n").unwrap();
        fs::create_dir_all(root.join("s1/subagents")).unwrap();
        let sub = p.join("-u-IDE-aiTerm-src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("s2.jsonl"), "{\"cwd\":\"/u/IDE/aiTerm/src\"}").unwrap();
        let sib = p.join("-u-IDE-aiTerm-backup");
        fs::create_dir_all(&sib).unwrap();
        fs::write(sib.join("s3.jsonl"), "{\"cwd\":\"/u/IDE/aiTerm-backup\"}\n").unwrap();

        fs::write(&s.h.claude_json[0], json!({ "projects": { "/u/IDE/aiTerm": { "hasTrustDialogAccepted": true }, "/u/other": {} }, "githubRepoPaths": { "o/r": ["/u/IDE/aiTerm"] } }).to_string()).unwrap();
        fs::write(&s.h.claude_json[1], json!({ "projects": { "/u/IDE/aiTerm/src": { "x": 1 }, "/u/IDE/maiterm/src": { "x": 2 } } }).to_string()).unwrap();
        fs::write(&s.h.claude_history[0], "{\"display\":\"hi\",\"project\":\"/u/IDE/aiTerm\"}\n{\"display\":\"x\",\"project\":\"/u/other\"}\n").unwrap();

        let plan = plan_in(&s.h, old, new);
        assert_eq!(plan.claude_sessions, 2);
        assert!(plan.claude_memory && plan.claude_trust);

        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let nr = p.join("-u-IDE-maiterm");
        assert!(nr.join("memory/MEMORY.md").exists() && nr.join("s1/subagents").is_dir());
        assert!(!root.exists());
        let t = fs::read_to_string(nr.join("s1.jsonl")).unwrap();
        assert!(t.ends_with("{\"relocatedCwd\":\"/u/IDE/maiterm\",\"sessionId\":\"s1\",\"type\":\"relocated\"}\n"), "{t}");
        // A transcript without a trailing newline still gets its record on a line of its own.
        let t2 = fs::read_to_string(p.join("-u-IDE-maiterm-src/s2.jsonl")).unwrap();
        assert_eq!(t2.lines().count(), 2);
        assert!(t2.contains("/u/IDE/maiterm/src"));
        assert!(sib.join("s3.jsonl").exists(), "a sibling sharing the slug prefix is another project");

        let j = read_json(&s.h.claude_json[0]).unwrap();
        assert_eq!(j["projects"]["/u/IDE/maiterm"]["hasTrustDialogAccepted"], true);
        assert!(j["projects"].get("/u/IDE/aiTerm").is_none());
        assert_eq!(j["githubRepoPaths"]["o/r"], json!(["/u/IDE/maiterm"]));
        let j2 = read_json(&s.h.claude_json[1]).unwrap();
        assert_eq!(j2["projects"]["/u/IDE/maiterm/src"]["x"], 2, "the entry already at the new key wins");
        let hist = fs::read_to_string(&s.h.claude_history[0]).unwrap();
        assert!(hist.contains("\"project\":\"/u/IDE/maiterm\"") && hist.contains("/u/other"));
    }

    /// A transcript and the prompt history hold `C:\\u\\a` (JSON-escaped), and Claude's config
    /// keys the same folder as `C:/u/a`: all three follow, each in its own form.
    #[cfg(windows)]
    #[test]
    fn windows_transcripts_history_and_trust_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new(r"C:\u\a"), Path::new(r"C:\u\b"));
        let p = &s.h.claude_projects;
        let d = p.join(claude_slug(r"C:\u\a"));
        fs::create_dir_all(d.join("memory")).unwrap();
        fs::write(d.join("memory/MEMORY.md"), "m").unwrap();
        fs::write(d.join("s1.jsonl"), format!("{}\n", json!({ "type": "user", "cwd": r"C:\u\a" }))).unwrap();
        fs::write(&s.h.claude_history[0], format!("{}\n", json!({ "display": "hi", "project": r"C:\u\a\src" }))).unwrap();
        fs::write(&s.h.claude_json[0], json!({ "projects": { "C:/u/a": { "hasTrustDialogAccepted": true } } }).to_string()).unwrap();

        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let n = p.join("C--u-b");
        assert!(n.join("memory/MEMORY.md").exists() && !d.exists());
        let t = fs::read_to_string(n.join("s1.jsonl")).unwrap();
        let last: Value = serde_json::from_str(t.lines().last().unwrap()).unwrap();
        assert_eq!(last["relocatedCwd"], r"C:\u\b");
        let hist: Value = serde_json::from_str(fs::read_to_string(&s.h.claude_history[0]).unwrap().trim()).unwrap();
        assert_eq!(hist["project"], r"C:\u\b\src");
        assert_eq!(read_json(&s.h.claude_json[0]).unwrap()["projects"]["C:/u/b"]["hasTrustDialogAccepted"], true);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_moved_before_is_found_by_its_relocated_record() {
        // Live-test bug: after a first move, line one still names the ORIGINAL folder, and
        // the second move found nothing of "its" project and left memory behind.
        let s = scratch();
        let d = s.h.claude_projects.join("-u-b");
        fs::create_dir_all(d.join("memory")).unwrap();
        fs::write(d.join("memory/MEMORY.md"), "m").unwrap();
        fs::write(
            d.join("s.jsonl"),
            "{\"cwd\":\"/u/a\"}\n{\"type\":\"relocated\",\"sessionId\":\"s\",\"relocatedCwd\":\"/u/b\"}\n{\"cwd\":\"/u/a\",\"x\":1}\n",
        )
        .unwrap();
        // And a new slug dir Claude already made, with an empty memory folder.
        fs::create_dir_all(s.h.claude_projects.join("-u-c/memory")).unwrap();
        let r = apply_in(&s.h, Path::new("/u/b"), Path::new("/u/c"), &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let n = s.h.claude_projects.join("-u-c");
        assert!(n.join("memory/MEMORY.md").exists());
        let t = fs::read_to_string(n.join("s.jsonl")).unwrap();
        assert!(t.trim_end().ends_with("\"relocatedCwd\":\"/u/c\",\"sessionId\":\"s\",\"type\":\"relocated\"}"), "{t}");
        assert!(!d.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stub_transcripts_without_a_folder_dont_hold_memory_back() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let d = s.h.claude_projects.join("-u-a");
        fs::create_dir_all(d.join("memory")).unwrap();
        fs::write(d.join("memory/MEMORY.md"), "m").unwrap();
        fs::write(d.join("real.jsonl"), "{\"cwd\":\"/u/a\"}\n").unwrap();
        fs::write(d.join("stub.jsonl"), "{\"type\":\"file-history-snapshot\"}\n").unwrap();
        assert!(plan_in(&s.h, old, new).claude_memory);
        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let n = s.h.claude_projects.join("-u-b");
        assert!(n.join("memory/MEMORY.md").exists() && n.join("real.jsonl").exists() && n.join("stub.jsonl").exists());
        assert!(!d.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_shared_slug_moves_only_our_sessions() {
        // `/u/p/web` (ours, a subfolder) and `/u/p-web` (a sibling project) share `-u-p-web`.
        let s = scratch();
        let (old, new) = (Path::new("/u/p"), Path::new("/u/q"));
        let d = s.h.claude_projects.join("-u-p-web");
        fs::create_dir_all(d.join("memory")).unwrap();
        fs::write(d.join("a.jsonl"), "{\"cwd\":\"/u/p-web\"}\n").unwrap();
        fs::write(d.join("b.jsonl"), "{\"cwd\":\"/u/p/web\"}\n").unwrap();
        let r = apply_in(&s.h, old, new, &[]);
        assert!(d.join("a.jsonl").exists(), "the sibling project's session stays");
        assert!(d.join("memory").is_dir(), "memory in a shared directory isn't ours to take");
        let moved = fs::read_to_string(s.h.claude_projects.join("-u-q-web/b.jsonl")).unwrap();
        assert!(moved.contains("\"relocatedCwd\":\"/u/q/web\""));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("memory"));
    }

    #[cfg(unix)]
    #[test]
    fn claude_merges_into_an_existing_project_and_reports_clashes() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let p = &s.h.claude_projects;
        fs::create_dir_all(p.join("-u-a/memory")).unwrap();
        fs::write(p.join("-u-a/s1.jsonl"), "{\"cwd\":\"/u/a\"}\n").unwrap();
        fs::write(p.join("-u-a/memory/MEMORY.md"), "old").unwrap();
        fs::write(p.join("-u-a/memory/topic.md"), "t").unwrap();
        fs::create_dir_all(p.join("-u-b/memory")).unwrap();
        fs::write(p.join("-u-b/memory/MEMORY.md"), "new").unwrap();
        let r = apply_in(&s.h, old, new, &[]);
        assert!(p.join("-u-b/s1.jsonl").exists());
        assert!(p.join("-u-b/memory/topic.md").exists());
        assert_eq!(fs::read_to_string(p.join("-u-b/memory/MEMORY.md")).unwrap(), "new");
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("MEMORY.md"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_stays_a_symlink() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let real = s.base.join("dotfiles-claude.json");
        fs::write(&real, json!({ "projects": { "/u/a": { "t": true } } }).to_string()).unwrap();
        std::os::unix::fs::symlink(&real, &s.h.claude_json[0]).unwrap();
        apply_in(&s.h, old, new, &[]);
        assert!(fs::symlink_metadata(&s.h.claude_json[0]).unwrap().file_type().is_symlink());
        assert_eq!(read_json(&real).unwrap()["projects"]["/u/b"]["t"], true);
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_edits_keep_untouched_lines_byte_for_byte() {
        let src = b"{\"b\":1,  \"a\":\"/u/a\"}\r\n{\"project\":\"/u/a/x\"}\nnot json /u/a\n{\"project\":\"/u/z\"}";
        let mut edit = |v: &mut Value| {
            let n = v.get("project").and_then(Value::as_str).and_then(|s| rebase(s, Path::new("/u/a"), Path::new("/u/b")));
            n.map(|n| v["project"] = Value::String(n)).is_some()
        };
        let out = String::from_utf8(edit_jsonl(src, "/u/a", &mut edit).unwrap()).unwrap();
        assert_eq!(out, "{\"b\":1,  \"a\":\"/u/a\"}\r\n{\"project\":\"/u/b/x\"}\nnot json /u/a\n{\"project\":\"/u/z\"}");
    }

    #[cfg(unix)]
    #[test]
    fn codex_trust_threads_and_rollouts_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let c = &s.h.codex;
        fs::write(c.join("config.toml"), "model = \"x\"\n\n[projects.\"/u/a\"]\ntrust_level = \"trusted\"\n\n[projects.\"/u/z\"]\ntrust_level = \"trusted\"\n").unwrap();
        let rollout = c.join("rollout.jsonl");
        fs::write(&rollout, "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/u/a\"}}\n{\"type\":\"response_item\",\"payload\":{\"text\":\"/u/a\"}}\n{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/u/a/src\"}}\n").unwrap();
        let db = rusqlite::Connection::open(c.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, cwd TEXT NOT NULL, rollout_path TEXT);").unwrap();
        db.execute("INSERT INTO threads VALUES ('t1', '/u/a', ?1), ('t2', '/u/z', NULL)", [rollout.to_string_lossy()]).unwrap();
        drop(db);

        assert_eq!(plan_in(&s.h, old, new).codex_sessions, 1);
        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let cfg = fs::read_to_string(c.join("config.toml")).unwrap();
        assert!(cfg.starts_with("model = \"x\"\n"), "the rest of the file keeps its layout: {cfg}");
        assert!(cfg.contains("[projects.\"/u/b\"]") && !cfg.contains("\"/u/a\"") && cfg.contains("/u/z"));
        let ro = fs::read_to_string(&rollout).unwrap();
        assert!(ro.contains("\"cwd\":\"/u/b\"") && ro.contains("\"cwd\":\"/u/b/src\""));
        assert!(ro.contains("\"text\":\"/u/a\""), "only session_meta / turn_context cwd is ours to change");
        let db = rusqlite::Connection::open(c.join("state_5.sqlite")).unwrap();
        let cwd: String = db.query_row("SELECT cwd FROM threads WHERE id='t1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cwd, "/u/b");
    }

    #[cfg(unix)]
    #[test]
    fn gemini_registry_markers_hash_dirs_and_trust_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let g = &s.h.gemini;
        fs::write(g.join("projects.json"), json!({ "projects": { "/u/a": "a" } }).to_string()).unwrap();
        fs::create_dir_all(g.join("tmp/a")).unwrap();
        fs::write(g.join("tmp/a/.project_root"), "/u/a").unwrap();
        fs::create_dir_all(g.join("tmp").join(sha256_hex("/u/a/sub"))).unwrap();
        fs::write(g.join("trustedFolders.json"), json!({ "/u/a": "TRUST_FOLDER" }).to_string()).unwrap();
        let r = apply_in(&s.h, old, new, &["/u/a/sub".into()]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(read_json(&g.join("projects.json")).unwrap()["projects"]["/u/b"], "a");
        assert_eq!(fs::read_to_string(g.join("tmp/a/.project_root")).unwrap(), "/u/b");
        assert!(g.join("tmp").join(sha256_hex("/u/b/sub")).is_dir());
        assert_eq!(read_json(&g.join("trustedFolders.json")).unwrap()["/u/b"], "TRUST_FOLDER");
    }

    fn create_agy_summary_db(path: &Path, rows: &[(&str, &str, &[u8])]) {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE conversation_summaries (
                conversation_id TEXT PRIMARY KEY,
                workspace_uris TEXT NOT NULL,
                raw_summary BLOB
            );",
        )
        .unwrap();
        for (id, uris, blob) in rows {
            db.execute(
                "INSERT INTO conversation_summaries (conversation_id, workspace_uris, raw_summary) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, uris, blob],
            )
            .unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_trust_history_caches_and_summaries_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new("/u/IDE/aiTerm"), Path::new("/u/IDE/maiterm"));
        let agy = s.h.gemini.join("antigravity-cli");
        fs::create_dir_all(agy.join("cache")).unwrap();

        // Symlinked settings.json: trust array has old, subfolder, already-present new (dedup),
        // sibling sharing the prefix, and unrelated path; permissions must stay untouched.
        let real_settings = s.base.join("dotfiles-agy-settings.json");
        fs::write(
            &real_settings,
            json!({
                "permissions": { "allow": ["mcp(maiterm/*)"] },
                "trustedWorkspaces": [
                    "/u/IDE/maiterm",
                    "/u/IDE/aiTerm",
                    "/u/IDE/aiTerm/src",
                    "/u/IDE/aiTerm-backup",
                    "/u/other"
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&real_settings, agy.join("settings.json")).unwrap();

        // history.jsonl: root, subfolder, sibling, unrelated, and a non-JSON line kept verbatim.
        fs::write(
            agy.join("history.jsonl"),
            concat!(
                "{\"display\":\"root\",\"workspace\":\"/u/IDE/aiTerm\",\"conversationId\":\"c1\"}\n",
                "{\"display\":\"sub\",\"workspace\":\"/u/IDE/aiTerm/src\"}\n",
                "{\"display\":\"sib\",  \"workspace\":\"/u/IDE/aiTerm-backup\"}\r\n",
                "{\"display\":\"other\",\"workspace\":\"/u/other\"}\n",
                "not json /u/IDE/aiTerm\n",
            ),
        )
        .unwrap();

        // cache/last_conversations.json: root moves; subfolder collides with newer new/src (new wins); sibling stays.
        fs::write(
            agy.join("cache/last_conversations.json"),
            json!({
                "/u/IDE/aiTerm": "c1",
                "/u/IDE/aiTerm/src": "c-old-sub",
                "/u/IDE/maiterm/src": "c-new-sub",
                "/u/IDE/aiTerm-backup": "c-sib"
            })
            .to_string(),
        )
        .unwrap();

        // cache/conversation_metadata.json: root, multi-URI with duplicate after rebase + cloud URI,
        // sibling, and internal entry with no summary.
        fs::write(
            agy.join("cache/conversation_metadata.json"),
            json!({
                "conversations": {
                    "c1": {
                        "summary": {
                            "ID": "c1",
                            "WorkspaceURIs": ["file:///u/IDE/aiTerm"]
                        }
                    },
                    "c2": {
                        "summary": {
                            "ID": "c2",
                            "WorkspaceURIs": [
                                "file:///u/IDE/maiterm/src",
                                "file:///u/IDE/aiTerm/src",
                                "https://cloud.example/proj"
                            ]
                        }
                    },
                    "c-sib": {
                        "summary": {
                            "ID": "c-sib",
                            "WorkspaceURIs": ["file:///u/IDE/aiTerm-backup"]
                        }
                    },
                    "c-internal": {
                        "is_internal": true
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        // conversation_summaries.db: c1, c2 (with raw_summary blob that must remain untouched), and sibling.
        let raw_blob: &[u8] = b"\x0a\x10opaque-protobuf-bytes";
        create_agy_summary_db(
            &agy.join("conversation_summaries.db"),
            &[
                ("c1", r#"["file:///u/IDE/aiTerm"]"#, raw_blob),
                ("c2", r#"["file:///u/IDE/aiTerm/src","file:///u/other"]"#, raw_blob),
                ("c-sib", r#"["file:///u/IDE/aiTerm-backup"]"#, raw_blob),
            ],
        );

        let plan = plan_in(&s.h, old, new);
        assert!(plan.agy_trust);
        // Unique IDs across DB, metadata, and last_conversations: c1, c2, c-old-sub.
        assert_eq!(plan.agy_sessions, 3);

        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.done.iter().any(|d| d.contains("Antigravity: folder trust moved")), "{:?}", r.done);
        assert!(r.done.iter().any(|d| d.contains("Antigravity: 3 session(s) moved")), "{:?}", r.done);

        // Symlink preserved and trustedWorkspaces deduplicated.
        assert!(fs::symlink_metadata(agy.join("settings.json")).unwrap().file_type().is_symlink());
        let st = read_json(&real_settings).unwrap();
        assert_eq!(st["permissions"]["allow"], json!(["mcp(maiterm/*)"]));
        assert_eq!(
            st["trustedWorkspaces"],
            json!(["/u/IDE/maiterm", "/u/IDE/maiterm/src", "/u/IDE/aiTerm-backup", "/u/other"])
        );

        // history.jsonl: matching lines rebased; untouched lines kept byte-for-byte.
        let hist = fs::read_to_string(agy.join("history.jsonl")).unwrap();
        assert!(hist.contains("\"workspace\":\"/u/IDE/maiterm\""));
        assert!(hist.contains("\"workspace\":\"/u/IDE/maiterm/src\""));
        assert!(hist.contains("{\"display\":\"sib\",  \"workspace\":\"/u/IDE/aiTerm-backup\"}\r\n"));
        assert!(hist.contains("not json /u/IDE/aiTerm\n"));

        // cache/last_conversations.json: root moved, existing new/src kept, sibling untouched.
        let lc = read_json(&agy.join("cache/last_conversations.json")).unwrap();
        assert_eq!(lc["/u/IDE/maiterm"], "c1");
        assert!(lc.get("/u/IDE/aiTerm").is_none());
        assert_eq!(lc["/u/IDE/maiterm/src"], "c-new-sub");
        assert!(lc.get("/u/IDE/aiTerm/src").is_none());
        assert_eq!(lc["/u/IDE/aiTerm-backup"], "c-sib");

        // cache/conversation_metadata.json: URIs rebased & deduped, cloud & sibling untouched.
        let meta = read_json(&agy.join("cache/conversation_metadata.json")).unwrap();
        assert_eq!(meta["conversations"]["c1"]["summary"]["WorkspaceURIs"], json!(["file:///u/IDE/maiterm"]));
        assert_eq!(
            meta["conversations"]["c2"]["summary"]["WorkspaceURIs"],
            json!(["file:///u/IDE/maiterm/src", "https://cloud.example/proj"])
        );
        assert_eq!(
            meta["conversations"]["c-sib"]["summary"]["WorkspaceURIs"],
            json!(["file:///u/IDE/aiTerm-backup"])
        );
        assert_eq!(meta["conversations"]["c-internal"]["is_internal"], true);

        // conversation_summaries.db: workspace_uris updated, sibling untouched, raw_summary untouched.
        let db = rusqlite::Connection::open(agy.join("conversation_summaries.db")).unwrap();
        let (u1, b1): (String, Vec<u8>) = db
            .query_row("SELECT workspace_uris, raw_summary FROM conversation_summaries WHERE conversation_id='c1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(u1, r#"["file:///u/IDE/maiterm"]"#);
        assert_eq!(b1, raw_blob);
        let u2: String = db
            .query_row("SELECT workspace_uris FROM conversation_summaries WHERE conversation_id='c2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(u2, r#"["file:///u/IDE/maiterm/src","file:///u/other"]"#);
        let usib: String = db
            .query_row("SELECT workspace_uris FROM conversation_summaries WHERE conversation_id='c-sib'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(usib, r#"["file:///u/IDE/aiTerm-backup"]"#);
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_percent_encoded_spaces_in_file_uris_roundtrip() {
        let s = scratch();
        let (old, new) = (Path::new("/u/IDE/website 2025"), Path::new("/u/IDE/website 2026"));
        let agy = s.h.gemini.join("antigravity-cli");
        fs::create_dir_all(agy.join("cache")).unwrap();

        fs::write(
            agy.join("settings.json"),
            json!({ "trustedWorkspaces": ["/u/IDE/website 2025/static/images"] }).to_string(),
        )
        .unwrap();
        fs::write(
            agy.join("history.jsonl"),
            "{\"display\":\"img\",\"workspace\":\"/u/IDE/website 2025/static/images\"}\n",
        )
        .unwrap();
        fs::write(
            agy.join("cache/last_conversations.json"),
            json!({ "/u/IDE/website 2025/static/images": "c-img" }).to_string(),
        )
        .unwrap();
        fs::write(
            agy.join("cache/conversation_metadata.json"),
            json!({
                "conversations": {
                    "c-img": {
                        "summary": {
                            "WorkspaceURIs": ["file:///u/IDE/website%202025/static/images"]
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        create_agy_summary_db(
            &agy.join("conversation_summaries.db"),
            &[("c-img", r#"["file:///u/IDE/website%202025/static/images"]"#, b"")],
        );

        let plan = plan_in(&s.h, old, new);
        assert!(plan.agy_trust);
        assert_eq!(plan.agy_sessions, 1);

        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        assert_eq!(
            read_json(&agy.join("settings.json")).unwrap()["trustedWorkspaces"],
            json!(["/u/IDE/website 2026/static/images"])
        );
        assert!(fs::read_to_string(agy.join("history.jsonl"))
            .unwrap()
            .contains("\"workspace\":\"/u/IDE/website 2026/static/images\""));
        assert_eq!(
            read_json(&agy.join("cache/last_conversations.json")).unwrap()["/u/IDE/website 2026/static/images"],
            "c-img"
        );
        assert_eq!(
            read_json(&agy.join("cache/conversation_metadata.json")).unwrap()["conversations"]["c-img"]["summary"]["WorkspaceURIs"],
            json!(["file:///u/IDE/website%202026/static/images"])
        );
        let db = rusqlite::Connection::open(agy.join("conversation_summaries.db")).unwrap();
        let uris: String = db
            .query_row("SELECT workspace_uris FROM conversation_summaries WHERE conversation_id='c-img'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uris, r#"["file:///u/IDE/website%202026/static/images"]"#);
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_missing_empty_or_corrupt_db_handled_cleanly() {
        let s = scratch();
        let (old, new) = (Path::new("/u/a"), Path::new("/u/b"));
        let agy = s.h.gemini.join("antigravity-cli");
        fs::create_dir_all(&agy).unwrap();
        fs::write(agy.join("settings.json"), json!({ "trustedWorkspaces": ["/u/a"] }).to_string()).unwrap();

        // Missing DB must not be created.
        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(!agy.join("conversation_summaries.db").exists());

        // Empty SQLite DB with no conversation_summaries table produces no warning.
        let empty_db = rusqlite::Connection::open(agy.join("conversation_summaries.db")).unwrap();
        empty_db.execute_batch("CREATE TABLE unrelated (id TEXT);").unwrap();
        drop(empty_db);
        let r = apply_in(&s.h, Path::new("/u/b"), Path::new("/u/c"), &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(read_json(&agy.join("settings.json")).unwrap()["trustedWorkspaces"], json!(["/u/c"]));

        // Corrupt DB warns without blocking settings.json update.
        fs::write(agy.join("conversation_summaries.db"), b"not a sqlite database").unwrap();
        let r = apply_in(&s.h, Path::new("/u/c"), Path::new("/u/d"), &[]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("conversation_summaries.db"));
        assert_eq!(read_json(&agy.join("settings.json")).unwrap()["trustedWorkspaces"], json!(["/u/d"]));
    }

    #[cfg(windows)]
    #[test]
    fn windows_antigravity_paths_and_file_uris_follow_the_folder() {
        let s = scratch();
        let (old, new) = (Path::new(r"C:\u\a"), Path::new(r"C:\u\b"));
        let agy = s.h.gemini.join("antigravity-cli");
        fs::create_dir_all(agy.join("cache")).unwrap();
        fs::write(agy.join("settings.json"), json!({ "trustedWorkspaces": [r"C:\u\a"] }).to_string()).unwrap();
        fs::write(agy.join("history.jsonl"), format!("{}\n", json!({ "display": "hi", "workspace": r"C:\u\a\src" }))).unwrap();
        fs::write(agy.join("cache/last_conversations.json"), json!({ r"C:\u\a": "c1" }).to_string()).unwrap();
        create_agy_summary_db(&agy.join("conversation_summaries.db"), &[("c1", r#"["file:///C:/u/a"]"#, b"")]);

        let plan = plan_in(&s.h, old, new);
        assert!(plan.agy_trust);
        assert_eq!(plan.agy_sessions, 1);

        let r = apply_in(&s.h, old, new, &[]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(read_json(&agy.join("settings.json")).unwrap()["trustedWorkspaces"], json!([r"C:\u\b"]));
        let hist: Value = serde_json::from_str(fs::read_to_string(agy.join("history.jsonl")).unwrap().trim()).unwrap();
        assert_eq!(hist["workspace"], r"C:\u\b\src");
        assert_eq!(read_json(&agy.join("cache/last_conversations.json")).unwrap()[r"C:\u\b"], "c1");
        let db = rusqlite::Connection::open(agy.join("conversation_summaries.db")).unwrap();
        let uris: String = db
            .query_row("SELECT workspace_uris FROM conversation_summaries WHERE conversation_id='c1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uris, r#"["file:///C:/u/b"]"#);
    }
}
