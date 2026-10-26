//! The fail-closed PreToolUse gate every Hive tab carries (docs/hive.md §6.8, §7.3).
//!
//! A speed bump and an audit log, not a wall: spike 9 ran a split string (`c=car; ${c}go`),
//! `base64 -d | sh` and a script written then run straight past a hook like this one, and no
//! pattern match on a command line closes that list. What it catches is the obvious forms and
//! their wrappers (`sh -c`, `bash -lc`, `env X=…`, aliases, full paths, `git -C`/`-c`), and
//! every refusal is logged. What actually stops a worker's push is that no remote is reachable
//! (§7.3); what stops its write is the sandbox. Nothing else in the design leans on this for
//! pushes.
//!
//! The coordinator is a normal agent tab by the human's decision (2026-10-26): unsandboxed, its
//! shell is its human's, and its gate refuses only a push, a remote or a merged pull request,
//! wrapped or not. Its Write and Edit don't reach the gate at all.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::paths::HivePaths;
use super::record::Hive;

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Allow,
    Deny(String),
}

/// What one Hive agent may touch through the tools the gate sees.
#[derive(Debug, Clone)]
pub struct GateScope {
    pub role: String,
    /// A worker's: its writes, its git and its queued commands are held to the lists below. The
    /// coordinator's isn't: only a push, a remote or a merge is refused it.
    pub contained: bool,
    /// Where Write and Edit may land, by real path.
    pub write_roots: Vec<PathBuf>,
    /// Where `git -C`, `--git-dir` and `--work-tree` may point.
    pub git_roots: Vec<PathBuf>,
    /// A worker's: the queued commands' words (`run::queued_prefixes`), which it runs through
    /// `hiveRun`, never directly (§6.8).
    pub queued: Vec<Vec<String>>,
}

/// In every queued-command refusal: `decide_event` puts those on the run log too.
pub const QUEUED_DENY: &str = "is queued in this Hive";

const PUSH_REFUSAL: &str = "No agent in a Hive pushes: landed work is pushed by maiTerm itself, and only when the Hive allows it.";
const REMOTE_REFUSAL: &str = "Hive agents can't use git remote: remotes are maiTerm's to manage.";

/// The coordinator's (§7.1, 2026-10-26): a normal agent tab's shell, but for a push.
pub fn coordinator_scope() -> GateScope {
    GateScope { role: super::ROLE_COORDINATOR.to_string(), contained: false, write_roots: Vec::new(), git_roots: Vec::new(), queued: Vec::new() }
}

/// A worker writes its own worktrees, its `scratch/` and the memory folder; its git runs in its
/// own worktrees and `scratch/` only, never `git -C` into a human checkout or another agent's
/// worktree. The git options and variables that run a command stay refused (`check_git`); its
/// own commits pass. A repo nested in its own folders is its own, and its git runs inside its
/// sandbox, so that isn't refused.
pub fn worker_scope(paths: &HivePaths, w: &super::worker::WorkerRecord) -> GateScope {
    let worktrees: Vec<PathBuf> = w.git_folders().filter_map(|f| f.worktree.as_ref()).map(PathBuf::from).collect();
    let scratch = paths.worker_scratch(&w.slug);
    let mut write_roots = worktrees.clone();
    write_roots.extend([scratch.clone(), paths.memory()]);
    let mut git_roots = worktrees;
    git_roots.push(scratch);
    GateScope { role: super::ROLE_WORKER.to_string(), contained: true, write_roots, git_roots, queued: Vec::new() }
}

/// The queued command `word args…` starts with, word by word, if any. Programs compare by
/// basename on both sides, so `/usr/bin/cargo test` is `cargo test` and `scripts/e2e.sh` is
/// `./scripts/e2e.sh`; a queued path matches only a word run by path, never a bare `test` that
/// the shell finds on PATH.
fn queued_match<'a>(word: &str, args: &[String], queued: &'a [Vec<String>]) -> Option<&'a Vec<String>> {
    let prog = program(word);
    let by_path = |w: &str| w.contains(['/', '\\']);
    queued.iter().find(|q| {
        q.first().is_some_and(|p| program(p) == prog && (!by_path(p) || by_path(word)))
            && q.len() - 1 <= args.len()
            && q[1..].iter().zip(args).all(|(a, b)| a == b)
    })
}

/// Names under a write root no agent's Write or Edit may create: project settings and MCP
/// servers Claude would load unseen, and a repo (or a worktree's `.git` pointer) whose config
/// Claude's own unsandboxed git would read. The deny rules hold these too; this reads the path.
const PROTECTED_NAMES: &[&str] = &[".claude", ".mcp.json", ".git"];

const MAX_DEPTH: usize = 8;

pub fn gate_decision(tool: &str, input: &Value, cwd: Option<&Path>, scope: &GateScope) -> GateDecision {
    let verdict = match tool {
        "Bash" | "PowerShell" | "Monitor" => match input.get("command").and_then(Value::as_str) {
            Some(cmd) => check_command(cmd, cwd, scope, 0),
            None => Err("maiTerm couldn't read this command, so it is blocked.".to_string()),
        },
        // The coordinator's hook doesn't send these (`COORDINATOR_GATE_MATCHER`): its writes are
        // its human's settings' to allow.
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" if !scope.contained => Ok(()),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let p = input.get("file_path").or_else(|| input.get("notebook_path")).and_then(Value::as_str);
            match p {
                Some(p) => check_write(p, cwd, scope),
                None => Err("maiTerm couldn't read the path this call writes, so it is blocked.".to_string()),
            }
        }
        _ => Ok(()),
    };
    match verdict {
        Ok(()) => GateDecision::Allow,
        Err(r) => GateDecision::Deny(r),
    }
}

fn check_write(p: &str, cwd: Option<&Path>, scope: &GateScope) -> Result<(), String> {
    let refuse = || {
        let roots: Vec<String> = scope.write_roots.iter().map(|r| r.display().to_string()).collect();
        format!("{p} is outside the folders this agent may write ({}).", roots.join(", "))
    };
    let real = resolve(cwd, p).ok_or_else(refuse)?;
    let Some(root) = scope.write_roots.iter().filter_map(|r| real_path(r)).find(|r| real.starts_with(r)) else {
        return Err(refuse());
    };
    let first = real.strip_prefix(&root).ok().and_then(|rest| rest.components().next()).map(|c| c.as_os_str().to_string_lossy().to_string());
    if first.is_some_and(|n| PROTECTED_NAMES.iter().any(|p| n.eq_ignore_ascii_case(p))) {
        return Err(format!("{p} is refused in a Hive: no agent writes .claude, .mcp.json or .git, which would run outside its sandbox."));
    }
    Ok(())
}

/// `p` against `base` (`~` expanded), with symlinks resolved as far as the path exists. None for
/// a relative path with no base, and for a dangling symlink, whose target can't be checked.
fn resolve(base: Option<&Path>, p: &str) -> Option<PathBuf> {
    let joined = if p == "~" {
        dirs::home_dir()?
    } else if let Some(rest) = p.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        base?.join(p)
    };
    real_path(&joined)
}

/// The real path of the longest prefix that exists, then the rest appended: a file about to be
/// created resolves through its folder's symlinks. None for a `..` past a missing folder (the
/// OS would refuse it anyway) and for a dangling symlink.
fn real_path(p: &Path) -> Option<PathBuf> {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let real = loop {
        match std::fs::canonicalize(&existing) {
            Ok(r) => break crate::share::canon(&r),
            Err(_) => {
                if std::fs::symlink_metadata(&existing).is_ok_and(|m| m.file_type().is_symlink()) {
                    return None;
                }
                rest.push(existing.file_name()?.to_os_string());
                existing = existing.parent()?.to_path_buf();
            }
        }
    };
    // `file_name` is None for a trailing `..`, so what's left holds only real names.
    let mut out = real;
    for part in rest.iter().rev() {
        out.push(part);
    }
    Some(out)
}

/// Commands this line runs, as words, plus the command strings nested in `$(…)` and backticks.
struct Lexed {
    segments: Vec<Vec<String>>,
    nested: Vec<String>,
}

fn take_parens(c: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    let mut depth = 1;
    let mut quote: Option<char> = None;
    while i < c.len() {
        match (quote, c[i]) {
            (Some(q), ch) if ch == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c[i]),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return (c[start..i].iter().collect(), i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    (c[start..].iter().collect(), c.len())
}

fn take_backticks(c: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    while i < c.len() && c[i] != '`' {
        i += 1;
    }
    (c[start..i].iter().collect(), (i + 1).min(c.len()))
}

fn lex(s: &str) -> Lexed {
    let c: Vec<char> = s.chars().collect();
    let mut out = Lexed { segments: Vec::new(), nested: Vec::new() };
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let flush_word = |cur: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(cur));
            *in_word = false;
        }
    };
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' => {
                if let Some(&n) = c.get(i + 1) {
                    if n != '\n' {
                        cur.push(n);
                        in_word = true;
                    }
                }
                i += 2;
            }
            '\'' => {
                in_word = true;
                i += 1;
                while i < c.len() && c[i] != '\'' {
                    cur.push(c[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < c.len() && c[i] != '"' {
                    match c[i] {
                        '\\' if matches!(c.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            cur.push(c[i + 1]);
                            i += 2;
                        }
                        '$' if c.get(i + 1) == Some(&'(') => {
                            let (inner, end) = take_parens(&c, i + 2);
                            out.nested.push(inner);
                            i = end;
                        }
                        '`' => {
                            let (inner, end) = take_backticks(&c, i + 1);
                            out.nested.push(inner);
                            i = end;
                        }
                        other => {
                            cur.push(other);
                            i += 1;
                        }
                    }
                }
                i += 1;
            }
            '$' if c.get(i + 1) == Some(&'(') => {
                let (inner, end) = take_parens(&c, i + 2);
                out.nested.push(inner);
                in_word = true;
                i = end;
            }
            '`' => {
                let (inner, end) = take_backticks(&c, i + 1);
                out.nested.push(inner);
                in_word = true;
                i = end;
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                flush_word(&mut cur, &mut in_word, &mut words);
                if !words.is_empty() {
                    out.segments.push(std::mem::take(&mut words));
                }
                i += 1;
            }
            '#' if !in_word => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ch if ch.is_whitespace() => {
                flush_word(&mut cur, &mut in_word, &mut words);
                i += 1;
            }
            other => {
                cur.push(other);
                in_word = true;
                i += 1;
            }
        }
    }
    flush_word(&mut cur, &mut in_word, &mut words);
    if !words.is_empty() {
        out.segments.push(words);
    }
    out
}

fn check_command(cmd: &str, cwd: Option<&Path>, scope: &GateScope, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("This command nests too deeply for maiTerm to check, so it is blocked.".to_string());
    }
    let lexed = lex(cmd);
    // After a `cd` the folder git runs in isn't known here (a subshell's `cd` doesn't even
    // last), so a git with no absolute `-C` is refused rather than placed by the event's cwd.
    let moves = lexed.segments.iter().flatten().any(|w| matches!(program(w), "cd" | "pushd" | "popd"));
    let cwd = if moves { None } else { cwd };
    for inner in &lexed.nested {
        check_command(inner, cwd, scope, depth + 1)?;
    }
    for seg in &lexed.segments {
        check_segment(seg, cwd, scope, depth)?;
    }
    Ok(())
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit()),
        None => false,
    }
}

fn program(w: &str) -> &str {
    w.rsplit(['/', '\\']).next().unwrap_or(w)
}

/// Options of a wrapper that take a value, so the value isn't read as the wrapped command.
fn wrapper_value_flags(wrapper: &str) -> Option<&'static [&'static str]> {
    Some(match wrapper {
        "env" => &["-u", "-C", "--unset", "--chdir"],
        "sudo" | "doas" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
        "xargs" => &["-I", "-n", "-P", "-L", "-s", "-E", "-d", "-a"],
        "nice" => &["-n"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "exec" => &["-a"],
        "command" | "nohup" | "builtin" | "time" | "caffeinate" | "stdbuf" | "unbuffer" => &[],
        _ => return None,
    })
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish", "pwsh", "powershell"];
const KEYWORDS: &[&str] = &["{", "}", "!", "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "time"];

/// Git's environment knobs that point it at another repo or config, or have it run a command.
/// Refused wherever they're set, since an exported one reaches every later git.
const GIT_ENV_REFUSED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "PAGER",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_EXEC_PATH",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "EDITOR",
    "VISUAL",
];

/// A worker's only: the coordinator's git runs its human's way (2026-10-26).
fn git_env_refusal(assignment: &str, scope: &GateScope) -> Option<String> {
    if !scope.contained {
        return None;
    }
    let (name, value) = assignment.split_once('=')?;
    // `GIT_EDITOR=true git rebase --continue` is how an agent gets past an editor.
    let harmless_editor = name.ends_with("EDITOR") && matches!(value, "true" | ":");
    (name.starts_with("GIT_CONFIG") || (GIT_ENV_REFUSED.contains(&name) && !harmless_editor))
        .then(|| format!("{name}=… is refused in a Hive: it points git at other config or has it run a command."))
}

fn check_segment(words: &[String], cwd: Option<&Path>, scope: &GateScope, depth: usize) -> Result<(), String> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if KEYWORDS.contains(&w) || is_assignment(w) {
            if let Some(r) = is_assignment(w).then(|| git_env_refusal(w, scope)).flatten() {
                return Err(r);
            }
            i += 1;
            continue;
        }
        let prog = program(w);
        if let Some(value_flags) = wrapper_value_flags(prog) {
            i += 1;
            // `env -S 'git push'` splits its argument into a command.
            if prog == "env" {
                if let Some(j) = words[i..].iter().position(|a| a == "-S" || a == "--split-string") {
                    if let Some(s) = words.get(i + j + 1) {
                        check_command(s, cwd, scope, depth + 1)?;
                    }
                }
            }
            while i < words.len() {
                let a = words[i].as_str();
                if prog == "env" && is_assignment(a) {
                    if let Some(r) = git_env_refusal(a, scope) {
                        return Err(r);
                    }
                }
                if value_flags.contains(&a) {
                    i += 2;
                } else if a.starts_with('-') || (prog == "env" && is_assignment(a)) {
                    i += 1;
                } else {
                    break;
                }
            }
            // `timeout 30 git push`: the duration comes first.
            if prog == "timeout" && i < words.len() {
                i += 1;
            }
            continue;
        }
        let args = &words[i + 1..];
        if let Some(q) = queued_match(w, args, &scope.queued) {
            // The queued word as the human wrote it: what hiveRun matches.
            let full: Vec<&str> = q.first().map(String::as_str).into_iter().chain(args.iter().map(String::as_str)).collect();
            return Err(format!(
                "`{}` {QUEUED_DENY}: run it with hiveRun {{command: \"{}\"}}; it's scheduled with the machine's other runs and becomes a \
                 receipt.",
                q.join(" "),
                full.join(" ").replace('"', "'")
            ));
        }
        return match prog {
            p if SHELLS.contains(&p) => match shell_command_string(args) {
                Some(s) => check_command(s, cwd, scope, depth + 1),
                None => Ok(()),
            },
            "eval" => check_command(&args.join(" "), cwd, scope, depth + 1),
            "alias" => {
                for a in args {
                    if let Some((_, v)) = a.split_once('=') {
                        check_command(v, cwd, scope, depth + 1)?;
                    }
                }
                Ok(())
            }
            "export" | "declare" | "typeset" | "readonly" | "local" => match args.iter().find_map(|a| git_env_refusal(a, scope)) {
                Some(r) => Err(r),
                None => Ok(()),
            },
            "git" => check_git(args, cwd, scope),
            "gh" => check_gh(args),
            _ => Ok(()),
        };
    }
    Ok(())
}

/// The script a shell runs with `-c` (or a flag cluster holding `c`, `-lc`), if any.
fn shell_command_string(args: &[String]) -> Option<&str> {
    let mut has_c = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            i += 1;
            break;
        }
        if a == "-Command" || a == "-c" && !has_c {
            has_c = true;
            i += 1;
            continue;
        }
        if a == "-o" || a == "+o" {
            i += 2;
            continue;
        }
        if let Some(flags) = a.strip_prefix('-').filter(|f| !f.starts_with('-')) {
            has_c |= flags.contains('c');
            i += 1;
            continue;
        }
        if a.starts_with("--") {
            i += 1;
            continue;
        }
        break;
    }
    if has_c {
        args.get(i).map(String::as_str)
    } else {
        None
    }
}

/// Options after the verb that run a configured command, write a file, or read one from
/// anywhere (`--no-index` takes any two paths).
const GIT_VERB_OPTS_REFUSED: &[&str] =
    &["--ext-diff", "--textconv", "--output", "--no-index", "--upload-pack", "--receive-pack", "--exec", "--open-files-in-pager"];

/// Real options that are prefixes of a refused one: an exact name wins over an abbreviation.
const GIT_VERB_OPTS_PREFIX_OK: &[&str] = &["--text"];

/// Whether `o` names a refused option. parse-options takes any unambiguous prefix of a long
/// option, so `git grep --op=<cmd>` is `--open-files-in-pager=<cmd>` (the first live test's
/// review): every `--<prefix>[=…]` of a refused name is refused.
fn is_refused_long_opt(o: &str) -> bool {
    let name = o.split('=').next().unwrap_or(o);
    name.len() > 2
        && name.starts_with("--")
        && !GIT_VERB_OPTS_PREFIX_OK.contains(&name)
        && GIT_VERB_OPTS_REFUSED.iter().any(|r| r.starts_with(name))
}

/// `git grep -O[<cmd>]` runs a pager command on the matches, attached or in a short-flag cluster.
fn is_grep_pager_flag(a: &str) -> bool {
    a.strip_prefix('-').is_some_and(|f| !f.starts_with('-') && f.contains('O'))
}

/// What git verb `args` run, if one can be read: global options and their values skipped.
fn git_verb(args: &[String]) -> Option<&str> {
    let mut j = 0;
    while j < args.len() {
        match args[j].as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" | "--config-env" => j += 2,
            a if a.starts_with('-') => j += 1,
            a => return Some(a),
        }
    }
    None
}

/// Whether a git verb is one the gate can name: what isn't a plain word may expand into one
/// (`c=pu; git ${c}sh`) or into an option (`c=-c; git $c core.fsmonitor=… diff`).
fn plain_verb(a: &str) -> bool {
    !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn check_git(args: &[String], cwd: Option<&Path>, scope: &GateScope) -> Result<(), String> {
    if !scope.contained {
        return match git_verb(args) {
            Some("push" | "send-pack") => Err(PUSH_REFUSAL.to_string()),
            Some("remote") => Err(REMOTE_REFUSAL.to_string()),
            Some(a) if !plain_verb(a) => Err(format!("maiTerm can't tell which git command `{a}` is, so it is blocked.")),
            _ => Ok(()),
        };
    }
    let mut base: Option<PathBuf> = cwd.and_then(real_path);
    let roots: Vec<PathBuf> = scope.git_roots.iter().filter_map(|g| real_path(g)).collect();
    // The deepest root holding `r`: a Hive folder inside another is checked as itself.
    let root_of = |r: &Path| roots.iter().filter(|g| r.starts_with(g)).max_by_key(|g| g.components().count());
    let in_roots = |r: &Path| root_of(r).is_some();
    let check_path = |base: &Option<PathBuf>, p: &str, flag: &str| -> Result<PathBuf, String> {
        match resolve(base.as_deref(), p) {
            Some(r) if in_roots(&r) => Ok(r),
            _ => Err(format!("git {flag} {p} points outside this agent's folders, so it is blocked.")),
        }
    };
    // Git runs in the agent's own folders, never wherever a `cd` left it.
    let repo_check = |b: Option<&Path>| -> Result<(), String> {
        match b.and_then(|b| root_of(b)) {
            Some(_) => Ok(()),
            None => Err("git runs here only in your own worktrees and folders: name one with git -C <folder>.".to_string()),
        }
    };
    // Not just `alias.*`: `core.fsmonitor`, `diff.external` and their kin run any command, so a
    // `git -C <copy> -c core.fsmonitor=… diff` was general Bash under a diff rule (the 3e review).
    let config_refusal = || "git -c and --config-env are refused in a Hive: a config value can run a command (core.fsmonitor, diff.external) or rename push (alias.*).".to_string();
    let mut j = 0;
    while j < args.len() {
        let a = args[j].as_str();
        match a {
            "-C" => {
                let p = args.get(j + 1).ok_or("git -C with no folder")?;
                base = Some(check_path(&base, p, "-C")?);
                j += 2;
            }
            "-c" | "--config-env" => return Err(config_refusal()),
            _ if a.starts_with("--config-env=") => return Err(config_refusal()),
            _ if a.starts_with("--exec-path=") => return Err("git --exec-path=… is refused in a Hive: it picks the programs git runs.".to_string()),
            "--git-dir" | "--work-tree" => {
                let p = args.get(j + 1).ok_or("a git option with no folder")?;
                check_path(&base, p, a)?;
                j += 2;
            }
            "--namespace" | "--super-prefix" => j += 2,
            _ if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => {
                let (flag, p) = a.split_once('=').unwrap_or((a, ""));
                check_path(&base, p, flag)?;
                j += 1;
            }
            _ if a.starts_with('-') => j += 1,
            "push" | "send-pack" => return Err(PUSH_REFUSAL.to_string()),
            "remote" => return Err(REMOTE_REFUSAL.to_string()),
            _ if !plain_verb(a) => {
                return Err(format!("maiTerm can't tell which git command `{a}` is, so it is blocked."));
            }
            _ => {
                repo_check(base.as_deref())?;
                let refused = args[j + 1..].iter().find(|o| is_refused_long_opt(o) || (a == "grep" && is_grep_pager_flag(o)));
                return match refused {
                    Some(o) => Err(format!("git {a} {o} is refused in a Hive: it runs a configured command, writes a file or reads one from anywhere.")),
                    None => Ok(()),
                };
            }
        }
    }
    Ok(())
}

fn check_gh(args: &[String]) -> Result<(), String> {
    let mut words = args.iter().map(String::as_str).filter(|a| !a.starts_with('-'));
    // `gh -R owner/repo pr merge`: a flag's value reads as a word here, so look for the pair.
    let mut prev = None;
    for w in words.by_ref() {
        if prev == Some("pr") && w == "merge" {
            return Err("Hive agents can't merge pull requests.".to_string());
        }
        prev = Some(w);
    }
    Ok(())
}

/// The gate route's whole decision for one PreToolUse event from `tab_id`, whose Hive agent
/// record (`hive::hive_agent`) is `agent`. Anything it can't place is a refusal: the gate fails
/// closed whatever goes wrong. Refusals are logged. Blocks on the filesystem.
pub fn decide_event(tab_id: &str, agent: Option<(Hive, String, HivePaths)>, body: &str) -> GateDecision {
    let Some((hive, role, paths)) = agent else {
        log::warn!("hive gate: tab {tab_id} isn't a Hive agent here; refused");
        return GateDecision::Deny("maiTerm doesn't know this tab as a Hive agent, so the call is blocked.".to_string());
    };
    let Ok(event) = serde_json::from_str::<Value>(body) else {
        return GateDecision::Deny("maiTerm couldn't read this call, so it is blocked.".to_string());
    };
    let tool = event.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let input = event.get("tool_input").cloned().unwrap_or(Value::Null);
    let cwd = event.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    let scope = if role == super::ROLE_WORKER {
        match super::worker::worker_for_tab(&paths, tab_id) {
            Some(w) => GateScope { queued: super::run::queued_prefixes(&hive), ..worker_scope(&paths, &w) },
            None => {
                log::warn!("hive {}: gate: no worker.json names tab {tab_id}; refused", hive.id);
                return GateDecision::Deny("maiTerm has no record of this worker, so the call is blocked.".to_string());
            }
        }
    } else if role == super::ROLE_COORDINATOR {
        coordinator_scope()
    } else {
        log::warn!("hive {}: gate: tab {tab_id} has no role \"{role}\" maiTerm knows; refused", hive.id);
        return GateDecision::Deny("maiTerm doesn't know this Hive agent's role, so the call is blocked.".to_string());
    };
    let decision = gate_decision(tool, &input, cwd.as_deref(), &scope);
    if let GateDecision::Deny(reason) = &decision {
        log::info!("hive {}: gate refused {tool} in tab {tab_id}: {reason}", hive.id);
        log_refusal(&paths, tab_id, tool, &describe(&input), reason);
        if reason.contains(QUEUED_DENY) {
            log_queued_deny(&paths, tab_id, &describe(&input));
        }
    }
    decision
}

/// A queued command run directly, on the run log beside the runs (§6.8): the audit trail of a
/// worker that went around the queue, or tried to.
pub fn log_queued_deny(paths: &HivePaths, tab_id: &str, command: &str) {
    let line = serde_json::json!({
        "type": "deny",
        "at": crate::commands::workspace::iso_now(),
        "tab_id": tab_id,
        "command": command.chars().take(500).collect::<String>(),
    });
    if let Err(e) = super::queue::append_line(&paths.run_log(), &line) {
        log::warn!("hive: queued-command refusal not on the run log: {e}");
    }
}

/// One line per refusal in `logs/gate.log`: the audit trail §6.8 promises.
pub fn log_refusal(paths: &HivePaths, tab_id: &str, tool: &str, what: &str, reason: &str) {
    let one_line = |s: &str, max: usize| s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect::<String>();
    let line = format!(
        "{} tab={} tool={} refused: {} | {}\n",
        crate::commands::workspace::iso_now(),
        tab_id,
        tool,
        one_line(reason, 300),
        one_line(what, 500)
    );
    let res = std::fs::create_dir_all(paths.logs()).and_then(|_| {
        std::fs::OpenOptions::new().create(true).append(true).open(paths.logs().join("gate.log"))?.write_all(line.as_bytes())
    });
    if let Err(e) = res {
        log::warn!("hive: gate log not written: {e}");
    }
}

/// What the call was, for the log: the command, or the path.
pub fn describe(input: &Value) -> String {
    ["command", "file_path", "notebook_path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    /// A scratch Hive: the coordinator's home and read copy, the memory folder, a worker's web
    /// worktree and scratch, another worker's worktree beside it, the human's checkout, and a
    /// folder outside it all. `scope` is the coordinator's, `worker` the worker's.
    struct Fx {
        base: PathBuf,
        paths: HivePaths,
        scope: GateScope,
        worker: GateScope,
        wt: PathBuf,
    }

    fn fx() -> Fx {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-gate-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let paths = HivePaths::new(base.join("hive"));
        for d in [paths.coordinator_home(), paths.memory(), paths.coordinator_worktree("web"), base.join("repo/web"), base.join("outside")] {
            fs::create_dir_all(d).unwrap();
        }
        let (w, worker) = worker_fx(&paths);
        let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
        Fx { base, paths, scope: coordinator_scope(), worker, wt }
    }

    /// The coordinator's shell, in its home.
    fn bash(f: &Fx, cmd: &str) -> GateDecision {
        gate_decision("Bash", &json!({ "command": cmd }), Some(&f.paths.coordinator_home()), &f.scope)
    }

    /// The worker's shell, in its web worktree.
    fn wbash(f: &Fx, cmd: &str) -> GateDecision {
        gate_decision("Bash", &json!({ "command": cmd }), Some(&f.wt), &f.worker)
    }

    /// Spike 9's table: what a hook like this caught, and the forms around it. Every Hive agent's
    /// gate refuses these, the coordinator's included (§7.3).
    #[test]
    fn pushes_and_their_wrappers_are_refused() {
        let f = fx();
        let wt = f.wt.clone();
        let refused = [
            "git push".to_string(),
            "git push origin main".into(),
            "sh -c 'git push'".into(),
            "bash -lc \"git push origin HEAD\"".into(),
            "env X=1 git push".into(),
            "env -i PATH=/usr/bin git push".into(),
            "env -S 'git push'".into(),
            "GIT_DIR=x git push".into(),
            "/usr/bin/git push".into(),
            "command git push".into(),
            "nohup git push &".into(),
            "timeout 30 git push".into(),
            "git -C . push".into(),
            format!("git -C '{}' push", wt.display()),
            "git -c x=y push".into(),
            "git --no-pager push".into(),
            "git send-pack ../r.git main".into(),
            "alias gp='git push'; gp".into(),
            "cd /tmp && git push".into(),
            "echo hi; git push".into(),
            "echo $(git push)".into(),
            "echo \"`git push`\"".into(),
            "true | git push".into(),
            "if true; then git push; fi".into(),
            "eval git push".into(),
            "git remote add x /tmp/x".into(),
            "git remote -v".into(),
            "gh pr merge 3 --squash".into(),
            "gh -R o/r pr merge 3".into(),
        ];
        for cmd in &refused {
            assert!(matches!(bash(&f, cmd), GateDecision::Deny(_)), "the coordinator should refuse: {cmd}");
            assert!(matches!(wbash(&f, cmd), GateDecision::Deny(_)), "a worker should refuse: {cmd}");
        }
        // A config value renames push, and only a worker's `-c` is refused: the coordinator's git
        // runs its human's way, so for it this is a speed bump with a known way round (§7.3).
        assert!(matches!(wbash(&f, "git -c alias.p=push p"), GateDecision::Deny(_)));
        assert_eq!(bash(&f, "git -c alias.p=push p"), GateDecision::Allow);
        let _ = fs::remove_dir_all(&f.base);
    }

    /// The human's decision (2026-10-26): the coordinator's shell is a normal agent tab's. Its
    /// git goes anywhere with any option, its tools run, and the gate passes its writes.
    // POSIX paths in shell words: a Windows path's backslashes are escapes to the parser.
    #[cfg(unix)]
    #[test]
    fn the_coordinator_runs_its_shell_like_any_agent_tab() {
        let f = fx();
        let (copy, out, human) = (f.paths.coordinator_worktree("web"), f.base.join("outside"), f.base.join("repo/web"));
        for cmd in [
            "git diff".to_string(),
            "git status # git push later".into(),
            "git log --grep push".into(),
            format!("git -C '{}' diff HEAD~1", copy.display()),
            format!("git -C '{}' -c core.pager=less log", out.display()),
            format!("cd '{}' && git commit -m 'fix the push button'", human.display()),
            format!("git --git-dir='{}/.git' log", human.display()),
            "GIT_PAGER=less git log".into(),
            "git config user.name x".into(),
            "git --bare log".into(),
            "git fetch origin".into(),
            "pnpm install --frozen-lockfile".into(),
            "npm run build && npm test".into(),
            "rm -rf node_modules".into(),
            "curl -s https://example.com".into(),
            "echo push".into(),
        ] {
            assert_eq!(bash(&f, &cmd), GateDecision::Allow, "should allow: {cmd}");
        }
        let home = f.paths.coordinator_home();
        for (tool, p) in [("Write", out.join("x")), ("Edit", human.join("src/main.rs")), ("Write", home.join(".claude/settings.json")), ("MultiEdit", copy.join("README.md"))] {
            assert_eq!(gate_decision(tool, &json!({ "file_path": p }), Some(&home), &f.scope), GateDecision::Allow, "{tool} {}", p.display());
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    /// The 3e review: git config runs commands, so `git -C <copy> -c core.fsmonitor=… diff`
    /// was general Bash under a diff rule. `diff.external`, a pager, `GIT_CONFIG_*` likewise; and
    /// parse-options takes any unambiguous prefix of a long option (the first live test's
    /// review), so `grep --op=<cmd>` is `--open-files-in-pager=<cmd>`. A worker's git runs none.
    #[test]
    fn git_config_and_command_options_are_refused() {
        let f = fx();
        let wt = f.wt.clone();
        let c = format!("git -C '{}'", wt.display());
        for cmd in [
            format!("{c} -c core.fsmonitor='touch x' diff"),
            format!("{c} -c diff.external='sh -c x' diff"),
            format!("{c} -c core.pager=x log"),
            format!("{c} -c color.ui=never log"),
            format!("{c} --config-env=core.fsmonitor=X diff"),
            format!("{c} --config-env core.fsmonitor=X diff"),
            format!("git --exec-path=/x -C '{}' diff", wt.display()),
            format!("{c} diff --ext-diff"),
            format!("{c} log -p --textconv"),
            format!("{c} diff --output=/tmp/x"),
            format!("{c} diff --output /tmp/x"),
            format!("{c} diff --no-index /etc/hosts /dev/null"),
            format!("GIT_EXTERNAL_DIFF=x {c} diff"),
            format!("GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=x {c} diff"),
            format!("env GIT_CONFIG_PARAMETERS=\"'core.fsmonitor'='x'\" {c} diff"),
            format!("export GIT_PAGER=x; {c} log"),
            format!("PAGER=x {c} log"),
            format!("GIT_EDITOR=vim {c} log"),
            format!("c=-c; {c} $c core.fsmonitor=x diff"),
            format!("{c} \"$(echo -c)\" core.fsmonitor=x diff"),
            format!("{c} '' diff"),
            format!("{c} fetch --upload-pack='sh -c x' . diff"),
            format!("{c} grep -O'sh -c x' -e diff"),
            format!("{c} grep --open-files-in-pager='sh -c x' -e diff"),
            format!("{c} grep --open='sh -c x' -e diff"),
            format!("{c} grep --op='sh -c x' -e diff"),
            format!("{c} grep --open-files='sh -c x' -e diff"),
            format!("{c} grep --open-files-in-pa='sh -c x' -e diff"),
            format!("{c} grep --open-files-in-pager -e diff"),
            format!("{c} diff --out=/tmp/x"),
            format!("{c} diff --ext"),
            format!("{c} diff --textc"),
            format!("{c} diff --no-ind /etc/passwd x"),
            format!("{c} grep -nOx -e diff"),
            format!("git -C '{}' -C .. diff", wt.display()),
        ] {
            assert!(matches!(wbash(&f, &cmd), GateDecision::Deny(_)), "should refuse: {cmd}");
        }
        // What review and work need still runs, and a worker's own git has every verb.
        for cmd in [
            format!("{c} grep -n -e diff"),
            format!("{c} status"),
            format!("{c} log -p --stat"),
            format!("{c} diff -O{}/order", wt.display()),
            format!("{c} diff --text --no-ext-diff"),
            format!("{c} grep --only-matching -e diff"),
            format!("{c} log --output-indicator-new=+"),
            format!("GIT_EDITOR=true {c} log"),
            "git commit -m x".into(),
            "git add -A".into(),
            "git --bare log".into(),
        ] {
            assert_eq!(wbash(&f, &cmd), GateDecision::Allow, "should allow: {cmd}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn git_pointed_outside_a_worker_s_folders_is_refused() {
        let f = fx();
        let out = f.base.join("outside");
        for cmd in [
            format!("git -C {} diff", out.display()),
            format!("git --git-dir={}/.git log", out.display()),
            format!("git --work-tree {} status", out.display()),
            "git -C ../../.. log".to_string(),
            "git -C /etc log".into(),
        ] {
            assert!(matches!(wbash(&f, &cmd), GateDecision::Deny(r) if r.contains("outside")), "{cmd}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    /// What spike 9 saw get past a hook like this. Not claimed: a worker's sandbox and its
    /// unreachable remotes are what hold these.
    #[test]
    fn the_known_bypasses_are_not_claimed() {
        let f = fx();
        // A verb built from a variable is refused now, but the program itself can be one.
        let deciders: [fn(&Fx, &str) -> GateDecision; 2] = [bash, wbash];
        for decide in deciders {
            assert!(matches!(decide(&f, "c=pu; git ${c}sh"), GateDecision::Deny(_)));
            assert_eq!(decide(&f, "c=pu; g=git; $g ${c}sh"), GateDecision::Allow);
            assert_eq!(decide(&f, "echo Z2l0IHB1c2g= | base64 -d | sh"), GateDecision::Allow);
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_worker_s_writes_land_only_in_its_folders_by_real_path() {
        let f = fx();
        let wt = f.wt.clone();
        let w = |p: &Path| gate_decision("Write", &json!({ "file_path": p }), Some(&wt), &f.worker);
        assert_eq!(w(&wt.join("plan.md")), GateDecision::Allow);
        assert_eq!(w(&wt.join("new/dir/plan.md")), GateDecision::Allow, "a folder that doesn't exist yet");
        assert_eq!(w(&f.paths.memory().join("decision.md")), GateDecision::Allow);
        assert_eq!(gate_decision("Edit", &json!({ "file_path": "notes.md" }), Some(&wt), &f.worker), GateDecision::Allow, "relative to cwd");
        assert!(matches!(w(&f.base.join("outside/x")), GateDecision::Deny(_)));
        assert!(matches!(w(&f.paths.coordinator_worktree("web").join("src.rs")), GateDecision::Deny(_)), "the coordinator's read copy");
        assert!(matches!(w(&wt.join("../../../../outside/x")), GateDecision::Deny(_)), "climbing out by ..");
        assert!(matches!(w(&wt.join("missing/../../../../../outside/x")), GateDecision::Deny(_)), "climbing out past a missing folder");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(f.base.join("outside"), wt.join("escape")).unwrap();
            assert!(matches!(w(&wt.join("escape/x")), GateDecision::Deny(_)), "a symlink out of its worktree");
            std::os::unix::fs::symlink(f.base.join("outside/nothing-here"), wt.join("dangling")).unwrap();
            assert!(matches!(w(&wt.join("dangling")), GateDecision::Deny(_)), "a dangling symlink can't be checked");
        }
        assert!(matches!(gate_decision("Write", &json!({}), Some(&wt), &f.worker), GateDecision::Deny(_)));
        assert_eq!(gate_decision("Read", &json!({ "file_path": "/etc/passwd" }), Some(&wt), &f.worker), GateDecision::Allow, "not the gate's");
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_event_the_gate_cannot_place_is_refused() {
        let f = fx();
        let hive = crate::hive::launch::tests::sample_hive();
        let agent = |role: &str| Some((hive.clone(), role.to_string(), HivePaths::new(f.paths.root.clone())));
        let coordinator = || agent(crate::hive::ROLE_COORDINATOR);
        assert!(matches!(decide_event("t", None, r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#), GateDecision::Deny(_)), "not a Hive tab");
        assert!(matches!(decide_event("t", coordinator(), "{not json"), GateDecision::Deny(_)));
        assert!(matches!(decide_event("t", agent("visitor"), r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#), GateDecision::Deny(_)), "a role maiTerm doesn't know");
        let home = f.paths.coordinator_home().to_string_lossy().to_string();
        let out = f.base.join("outside").to_string_lossy().to_string();
        for cmd in ["git diff", "npm install", "git -c core.pager=less log"] {
            let ev = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}, "cwd": out}).to_string();
            assert_eq!(decide_event("t", coordinator(), &ev), GateDecision::Allow, "{cmd}");
        }
        let write = serde_json::json!({"tool_name": "Write", "tool_input": {"file_path": format!("{out}/x")}, "cwd": home}).to_string();
        assert_eq!(decide_event("t", coordinator(), &write), GateDecision::Allow);
        let push = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git push"}, "cwd": home}).to_string();
        assert!(matches!(decide_event("t", coordinator(), &push), GateDecision::Deny(_)));
        assert!(fs::read_to_string(f.paths.logs().join("gate.log")).unwrap().contains("git push"), "the refusal is on the record");
        let _ = fs::remove_dir_all(&f.base);
    }

    /// A worker of the fixture's Hive with real folders: its web worktree and scratch, another
    /// worker's worktree beside it.
    fn worker_fx(paths: &HivePaths) -> (crate::hive::worker::WorkerRecord, GateScope) {
        for d in [paths.worker_worktree("auth", "web"), paths.worker_scratch("auth"), paths.worker_worktree("billing", "web")] {
            fs::create_dir_all(d).unwrap();
        }
        let w = crate::hive::worker::WorkerRecord {
            task_id: "t-auth".into(),
            slug: "auth".into(),
            tab_id: Some("tab-w".into()),
            folders: vec![crate::hive::worker::WorkerFolder {
                name: "web".into(),
                kind: "git".into(),
                worktree: Some(paths.worker_worktree("auth", "web").to_string_lossy().to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let scope = worker_scope(paths, &w);
        (w, scope)
    }

    #[test]
    fn a_worker_writes_and_runs_git_only_in_its_own_folders() {
        let f = fx();
        let (_, scope) = worker_fx(&f.paths);
        let wt = f.paths.worker_worktree("auth", "web");
        let human = f.base.join("repo/web");
        let w = |p: &Path| gate_decision("Write", &json!({ "file_path": p }), Some(&wt), &scope);
        assert_eq!(w(&wt.join("src/lib.rs")), GateDecision::Allow);
        assert_eq!(w(&f.paths.worker_scratch("auth").join("render.png")), GateDecision::Allow);
        assert_eq!(w(&f.paths.memory().join("note.md")), GateDecision::Allow);
        for refused in [f.paths.worker_worktree("billing", "web").join("x"), human.join("x"), f.paths.coordinator_worktree("web").join("x"), f.paths.coordinator_home().join("x")] {
            assert!(matches!(w(&refused), GateDecision::Deny(_)), "{}", refused.display());
        }
        let bash = |cmd: &str| gate_decision("Bash", &json!({ "command": cmd }), Some(&wt), &scope);
        for ok in ["git add -A".to_string(), "git commit -m 'Pass the -c flag through'".into(), format!("git -C '{}' log --oneline", wt.display()), "npm test".into()] {
            assert_eq!(bash(&ok), GateDecision::Allow, "{ok}");
        }
        for refused in [
            format!("git -C '{}' log", human.display()),
            format!("git -C '{}' status", f.paths.worker_worktree("billing", "web").display()),
            format!("git -C '{}' diff", f.paths.coordinator_worktree("web").display()),
            "git push".into(),
            "git -c core.fsmonitor='touch x' status".into(),
            "GIT_CONFIG_COUNT=1 git status".into(),
            "git diff --ext-diff".into(),
            format!("cd '{}' && git commit -m x", human.display()),
        ] {
            assert!(matches!(bash(&refused), GateDecision::Deny(_)), "should refuse: {refused}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    /// The deny rules hold these too; the gate reads the real path, so a symlink or a relative
    /// path doesn't slip past a glob.
    #[test]
    fn no_worker_writes_claude_settings_mcp_servers_or_a_git_pointer() {
        let f = fx();
        let (_, scope) = worker_fx(&f.paths);
        let wt = f.paths.worker_worktree("auth", "web");
        let w = |p: &str| gate_decision("Write", &json!({ "file_path": p }), Some(&wt), &scope);
        for p in [".claude/settings.json", ".mcp.json", ".git", ".git/config", ".CLAUDE/settings.local.json"] {
            assert!(matches!(w(p), GateDecision::Deny(r) if r.contains("refused in a Hive")), "{p}");
        }
        assert!(matches!(w(&f.paths.worker_scratch("auth").join(".git/config").to_string_lossy()), GateDecision::Deny(_)));
        assert_eq!(w("docs/.claude-notes.md"), GateDecision::Allow, "only the names themselves, at the root");
        assert_eq!(w("packages/x/.gitignore"), GateDecision::Allow);
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_worker_the_gate_cannot_find_is_refused() {
        let f = fx();
        let (w, _) = worker_fx(&f.paths);
        let hive = crate::hive::launch::tests::sample_hive();
        let agent = || Some((hive.clone(), crate::hive::ROLE_WORKER.to_string(), HivePaths::new(f.paths.root.clone())));
        let wt = f.paths.worker_worktree("auth", "web").to_string_lossy().to_string();
        let commit = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git commit -m x"}, "cwd": wt}).to_string();
        assert!(matches!(decide_event("tab-w", agent(), &commit), GateDecision::Deny(r) if r.contains("no record of this worker")));
        crate::hive::worker::write_worker(&f.paths.task("t-auth"), &w).unwrap();
        assert_eq!(decide_event("tab-w", agent(), &commit), GateDecision::Allow);
        assert!(matches!(decide_event("another-tab", agent(), &commit), GateDecision::Deny(_)));
        let _ = fs::remove_dir_all(&f.base);
    }

    /// Spike 9's wrappers around a queued command: each is the same `cargo test` to the gate.
    #[test]
    fn a_worker_runs_queued_commands_only_through_hive_run() {
        let f = fx();
        let (w, scope) = worker_fx(&f.paths);
        let scope = GateScope { queued: vec![vec!["cargo".into(), "test".into()], vec!["npm".into(), "run".into(), "tauri:dev".into()]], ..scope };
        let wt = f.paths.worker_worktree("auth", "web");
        let bash = |cmd: &str| gate_decision("Bash", &json!({ "command": cmd }), Some(&wt), &scope);
        for cmd in [
            "cargo test",
            "cargo test -p core --release",
            "sh -c 'cargo test'",
            "bash -lc \"cargo test -p core\"",
            "env X=1 cargo test",
            "env -u HOME cargo test",
            "RUST_LOG=debug cargo test",
            "/usr/local/bin/cargo test",
            "/Users/x/.cargo/bin/cargo test",
            "command cargo test",
            "nohup cargo test &",
            "timeout 600 cargo test",
            "echo start && cargo test",
            "cd crates/core; cargo test",
            "echo $(cargo test)",
            "npm run tauri:dev",
            "eval cargo test",
        ] {
            match bash(cmd) {
                GateDecision::Deny(r) => assert!(r.contains(QUEUED_DENY) && r.contains("hiveRun"), "{cmd}: {r}"),
                GateDecision::Allow => panic!("should refuse: {cmd}"),
            }
        }
        for cmd in ["cargo build", "cargo check", "cargo testing", "echo cargo test", "npm run tauri:dev:isolated", "npm test", "git commit -m 'cargo test passes'"] {
            assert_eq!(bash(cmd), GateDecision::Allow, "not queued: {cmd}");
        }
        let GateDecision::Deny(r) = bash("env X=1 /usr/bin/cargo test -p core") else { panic!() };
        assert!(r.contains("`cargo test` is queued") && r.contains("command: \"cargo test -p core\""), "names the hiveRun to make: {r}");

        // A queued path matches however the script is reached by path, never a bare word on PATH.
        let paths_scope = GateScope { queued: vec![vec!["./scripts/e2e.sh".into()], vec!["bin/test".into()]], ..scope.clone() };
        let bash_p = |cmd: &str| gate_decision("Bash", &json!({ "command": cmd }), Some(&wt), &paths_scope);
        for cmd in ["./scripts/e2e.sh --all", "scripts/e2e.sh", "sh -c './scripts/e2e.sh --all'", "/abs/wt/bin/test -v", "bin/test"] {
            match bash_p(cmd) {
                GateDecision::Deny(r) => assert!(r.contains(QUEUED_DENY), "{cmd}: {r}"),
                GateDecision::Allow => panic!("should refuse: {cmd}"),
            }
        }
        let GateDecision::Deny(r) = bash_p("scripts/e2e.sh --all") else { panic!() };
        assert!(r.contains("command: \"./scripts/e2e.sh --all\""), "names the queued word: {r}");
        for cmd in ["test -f x", "e2e.sh", "echo ./scripts/e2e.sh"] {
            assert_eq!(bash_p(cmd), GateDecision::Allow, "not the queued script: {cmd}");
        }

        // From the event: the worker's scope carries the Hive's queued commands, and the refusal
        // is on the run log as well as the gate log.
        crate::hive::worker::write_worker(&f.paths.task("t-auth"), &w).unwrap();
        let mut hive = crate::hive::launch::tests::sample_hive();
        hive.settings.run_queue.commands = vec![crate::hive::record::QueuedCommand { folder: "web".into(), prefix: "cargo test".into(), ..Default::default() }];
        let agent = Some((hive.clone(), crate::hive::ROLE_WORKER.to_string(), HivePaths::new(f.paths.root.clone())));
        let ev = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "sh -c 'cargo test'"}, "cwd": wt}).to_string();
        assert!(matches!(decide_event("tab-w", agent.clone(), &ev), GateDecision::Deny(r) if r.contains(QUEUED_DENY)));
        let log = fs::read_to_string(f.paths.run_log()).unwrap();
        let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!((line["type"].as_str(), line["tab_id"].as_str(), line["command"].as_str()), (Some("deny"), Some("tab-w"), Some("sh -c 'cargo test'")));
        let push = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git push"}, "cwd": wt}).to_string();
        assert!(matches!(decide_event("tab-w", agent, &push), GateDecision::Deny(_)));
        assert_eq!(fs::read_to_string(f.paths.run_log()).unwrap().lines().count(), 1, "only queued-command refusals go on the run log");
        // The coordinator's shell runs what its human's would: nothing is queued for it.
        let coordinator = Some((hive, crate::hive::ROLE_COORDINATOR.to_string(), HivePaths::new(f.paths.root.clone())));
        assert_eq!(decide_event("coord", coordinator, &ev), GateDecision::Allow);
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn refusals_are_logged() {
        let f = fx();
        log_refusal(&f.paths, "tab-1", "Bash", "git push\norigin", "No agent pushes");
        let text = fs::read_to_string(f.paths.logs().join("gate.log")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("tab=tab-1 tool=Bash refused: No agent pushes | git push origin"));
        let _ = fs::remove_dir_all(&f.base);
    }
}
