//! Agent runtimes' per-project state (docs/relocate.md §3). Each runtime keys what it keeps per
//! project by the project's PATH, so a moved folder looks like a brand-new project to it: no
//! resumable sessions, no memory, the trust question again. This carries that state across.
//!
//! Every runtime is best effort and independent: by the time this runs the folder has already
//! moved, so one runtime failing must not stop the others, and is reported rather than raised.
//! All of it assumes no agent in the folder is running (`commands/relocate.rs` suspends them
//! first) — a live Claude appends to the transcript path it opened, and would recreate the old
//! project directory behind us.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use super::rebase;

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
    let dirs = claude_dirs(&h.claude_projects, old, new);
    AgentPlan {
        claude_sessions: dirs.iter().map(|d| sessions_in(&d.dir).len()).sum(),
        claude_memory: dirs.iter().any(|d| d.dir.join("memory").is_dir()),
        claude_trust: h.claude_json.iter().any(|f| read_json(f).is_some_and(|v| has_key_under(&v["projects"], old, new))),
        codex_sessions: codex_threads(&h.codex, old, new).len(),
        codex_trust: fs::read_to_string(h.codex.join("config.toml")).is_ok_and(|t| codex_trust_keys(&t, old, new) > 0),
        gemini_projects: read_json(&h.gemini.join("projects.json")).map_or(0, |v| keys_under(&v["projects"], old, new)),
    }
}

pub fn apply_in(h: &Homes, old: &Path, new: &Path, known: &[String]) -> AgentReport {
    let mut r = AgentReport::default();
    claude(h, old, new, &mut r);
    codex(h, old, new, &mut r);
    gemini(h, old, new, known, &mut r);
    r
}

// ── shared ──────────────────────────────────────────────────────────────────────────────────

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

/// Write beside, then rename over: a crash mid-write must never leave a runtime's config
/// truncated — a broken `.claude.json` loses every account setting, not just this project's.
fn write_atomic(p: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = p.with_extension(format!("maiterm-relocate-{}", std::process::id()));
    fs::write(&tmp, contents)?;
    if let Ok(m) = fs::metadata(p) {
        let _ = fs::set_permissions(&tmp, m.permissions());
    }
    fs::rename(&tmp, p).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

fn keys_under(map: &Value, old: &Path, new: &Path) -> usize {
    map.as_object().map_or(0, |m| m.keys().filter(|k| rebase(k, old, new).is_some()).count())
}

fn has_key_under(map: &Value, old: &Path, new: &Path) -> bool {
    keys_under(map, old, new) > 0
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

fn sessions_in(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

struct ClaudeDir {
    dir: PathBuf,
    /// The folder it belongs to, after the move.
    new_path: String,
}

/// Project directories that belong to `old` or a folder inside it. The slug is lossy (`a.b` and
/// `a-b` collide, and `old-sibling` shares the prefix), so a name match is only a candidate: a
/// session inside must have been started under `old`. A directory with no sessions (memory
/// alone) is taken only when its name is exactly `old`'s.
fn claude_dirs(projects: &Path, old: &Path, new: &Path) -> Vec<ClaudeDir> {
    let old_slug = claude_slug(&old.to_string_lossy());
    let mut out = vec![];
    for e in fs::read_dir(projects).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let candidate = if cfg!(target_os = "macos") {
            let (n, o) = (name.to_lowercase(), old_slug.to_lowercase());
            n == o || n.starts_with(&format!("{o}-"))
        } else {
            name == old_slug || name.starts_with(&format!("{old_slug}-"))
        };
        if !candidate || !e.path().is_dir() {
            continue;
        }
        let cwd = sessions_in(&e.path()).iter().find_map(|s| first_cwd(s));
        let new_path = match cwd {
            Some(c) => match rebase(&c, old, new) {
                Some(n) => n,
                None => continue,
            },
            None if name == old_slug => new.to_string_lossy().to_string(),
            None => continue,
        };
        out.push(ClaudeDir { dir: e.path(), new_path });
    }
    out
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
    let needs_nl = fs::read(jsonl).map(|b| b.last().is_some_and(|c| *c != b'\n')).unwrap_or(false);
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
                    let _ = fs::remove_file(&p);
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(to.join(rest), &p);
                }
            }
        } else if meta.is_dir() {
            repoint_links(&p, from, to);
        }
    }
}

fn claude(h: &Homes, old: &Path, new: &Path, r: &mut AgentReport) {
    let mut sessions = 0;
    let mut memory = false;
    for d in claude_dirs(&h.claude_projects, old, new) {
        let to = h.claude_projects.join(claude_slug(&d.new_path));
        // Case-only rename on a case-insensitive disk: the "destination" is the source itself.
        let same = to.exists() && fs::canonicalize(&to).ok() == fs::canonicalize(&d.dir).ok();
        let moved: Vec<PathBuf> = sessions_in(&d.dir).iter().filter_map(|s| s.file_name().map(|n| to.join(n))).collect();
        memory |= d.dir.join("memory").is_dir();
        let result = if !to.exists() || same {
            fs::rename(&d.dir, &to).map(|_| vec![])
        } else {
            merge_dir(&d.dir, &to)
        };
        match result {
            Ok(left) => {
                for p in &left {
                    r.warnings.push(format!(
                        "Claude: {} was already in the new project and was left in {} — merge it by hand.",
                        p.file_name().unwrap_or_default().to_string_lossy(),
                        d.dir.display()
                    ));
                }
                repoint_links(&to, &d.dir, &to);
                for s in moved.iter().filter(|s| s.exists()) {
                    let cwd = first_cwd(s).and_then(|c| rebase(&c, old, new)).unwrap_or_else(|| d.new_path.clone());
                    match append_relocated(s, &cwd) {
                        Ok(()) => sessions += 1,
                        Err(e) => r.warnings.push(format!("Claude: couldn't mark {} as moved: {e}", s.display())),
                    }
                }
            }
            Err(e) => r.warnings.push(format!("Claude: couldn't move {}: {e}", d.dir.display())),
        }
    }
    if sessions > 0 {
        r.done.push(format!("Claude: {sessions} session(s){} moved", if memory { " and project memory" } else { "" }));
    }

    let mut trust_files = 0;
    for f in &h.claude_json {
        let Some(mut v) = read_json(f) else { continue };
        let mut n = v.get_mut("projects").and_then(Value::as_object_mut).map_or(0, |m| rekey(m, old, new));
        if let Some(repos) = v.get_mut("githubRepoPaths").and_then(Value::as_object_mut) {
            for list in repos.values_mut().filter_map(Value::as_array_mut) {
                let mut seen = vec![];
                for p in list.iter_mut() {
                    if let Some(rb) = p.as_str().and_then(|s| rebase(s, old, new)) {
                        *p = Value::String(rb);
                        n += 1;
                    }
                }
                list.retain(|p| {
                    let keep = !seen.contains(p);
                    seen.push(p.clone());
                    keep
                });
            }
        }
        if n == 0 {
            continue;
        }
        match serde_json::to_vec_pretty(&v).map_err(std::io::Error::other).and_then(|b| write_atomic(f, &b)) {
            Ok(()) => trust_files += 1,
            Err(e) => r.warnings.push(format!("Claude: couldn't update {}: {e}", f.display())),
        }
    }
    if trust_files > 0 {
        r.done.push(format!("Claude: project settings and trust moved in {trust_files} config file(s)"));
    }

    for f in &h.claude_history {
        if let Err(e) = rewrite_jsonl(f, old, |v| {
            let p = v.get("project").and_then(Value::as_str).and_then(|s| rebase(s, old, new));
            p.map(|p| v["project"] = Value::String(p)).is_some()
        }) {
            r.warnings.push(format!("Claude: couldn't update prompt history {}: {e}", f.display()));
        }
    }
}

/// Rewrite a JSONL file line by line. Only lines that mention `old` are parsed; the rest are
/// copied byte for byte. `edit` returns whether it changed the line. Untouched when nothing did.
fn rewrite_jsonl(f: &Path, old: &Path, mut edit: impl FnMut(&mut Value) -> bool) -> std::io::Result<bool> {
    let Ok(file) = fs::File::open(f) else { return Ok(false) };
    let needle = old.to_string_lossy().to_string();
    let mut out: Vec<u8> = Vec::new();
    let mut changed = false;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let body = line.trim_end_matches(['\n', '\r']);
        let mut written = false;
        if body.contains(&needle) {
            if let Ok(mut v) = serde_json::from_str::<Value>(body) {
                if edit(&mut v) {
                    out.extend(serde_json::to_vec(&v)?);
                    out.extend(&line.as_bytes()[body.len()..]);
                    changed = true;
                    written = true;
                }
            }
        }
        if !written {
            out.extend(line.as_bytes());
        }
        line.clear();
    }
    if changed {
        write_atomic(f, &out)?;
    }
    Ok(changed)
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
    if let Ok(text) = fs::read_to_string(&cfg) {
        if let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() {
            let mut n = 0;
            if let Some(t) = doc.get_mut("projects").and_then(|p| p.as_table_like_mut()) {
                let moving: Vec<(String, String)> = t
                    .iter()
                    .filter_map(|(k, _)| rebase(k, old, new).filter(|nk| nk != k).map(|nk| (k.to_string(), nk)))
                    .collect();
                for (from, to) in moving {
                    if let Some(item) = t.remove(&from) {
                        if t.get(&to).is_none() {
                            t.insert(&to, item);
                        }
                        n += 1;
                    }
                }
            }
            if n > 0 {
                match write_atomic(&cfg, doc.to_string().as_bytes()) {
                    Ok(()) => r.done.push("Codex: project trust moved".into()),
                    Err(e) => r.warnings.push(format!("Codex: couldn't update {}: {e}", cfg.display())),
                }
            }
        }
    }

    // Sessions: the thread index's cwd, and the cwd each rollout records — `codex resume` asks
    // which folder to use when the recorded one is gone, which would stall an auto-resume.
    let threads = codex_threads(&h.codex, old, new);
    let mut moved = 0;
    for (db_path, id, cwd, rollout) in &threads {
        let Some(new_cwd) = rebase(cwd, old, new) else { continue };
        let db = rusqlite::Connection::open(db_path).and_then(|db| {
            db.busy_timeout(std::time::Duration::from_secs(5))?;
            db.execute("UPDATE threads SET cwd = ?1 WHERE id = ?2", rusqlite::params![new_cwd, id])
        });
        if let Err(e) = db {
            r.warnings.push(format!("Codex: couldn't update session {id}: {e}"));
            continue;
        }
        if !rollout.is_empty() {
            let res = rewrite_jsonl(Path::new(rollout), old, |v| {
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
    if let Some(mut v) = read_json(&reg) {
        if let Some(m) = v.get_mut("projects").and_then(Value::as_object_mut) {
            let slugs: Vec<String> = m
                .iter()
                .filter(|(k, _)| rebase(k, old, new).is_some())
                .filter_map(|(_, s)| s.as_str().map(String::from))
                .collect();
            moved += rekey(m, old, new);
            for slug in slugs {
                for base in ["tmp", "history"] {
                    let marker = g.join(base).join(&slug).join(".project_root");
                    if let Some(n) = fs::read_to_string(&marker).ok().and_then(|t| rebase(t.trim(), old, new)) {
                        if let Err(e) = write_atomic(&marker, n.as_bytes()) {
                            r.warnings.push(format!("Gemini: couldn't update {}: {e}", marker.display()));
                        }
                    }
                }
            }
        }
        if moved > 0 {
            if let Err(e) = serde_json::to_vec_pretty(&v).map_err(std::io::Error::other).and_then(|b| write_atomic(&reg, &b)) {
                r.warnings.push(format!("Gemini: couldn't update {}: {e}", reg.display()));
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
    if let Some(mut v) = read_json(&trust) {
        if v.as_object_mut().map_or(0, |m| rekey(m, old, new)) > 0 {
            if let Err(e) = serde_json::to_vec_pretty(&v).map_err(std::io::Error::other).and_then(|b| write_atomic(&trust, &b)) {
                r.warnings.push(format!("Gemini: couldn't update {}: {e}", trust.display()));
            }
        }
    }
    if moved > 0 {
        r.done.push(format!("Gemini: {moved} project(s) moved"));
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
}
