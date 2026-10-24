//! The fail-closed PreToolUse gate every Hive tab carries (docs/hive.md §6.8, §7.3).
//!
//! A speed bump and an audit log, not a wall: spike 9 ran a split string (`c=car; ${c}go`),
//! `base64 -d | sh` and a script written then run straight past a hook like this one, and no
//! pattern match on a command line closes that list. What it catches is the obvious forms and
//! their wrappers (`sh -c`, `bash -lc`, `env X=…`, aliases, full paths, `git -C`/`-c`), and
//! every refusal is logged. What actually stops a push is that no remote is reachable (§7.3);
//! what stops a write is the sandbox. Nothing else in the design leans on this.

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
    /// Where Write and Edit may land, by real path.
    pub write_roots: Vec<PathBuf>,
    /// Where `git -C`, `--git-dir` and `--work-tree` may point.
    pub git_roots: Vec<PathBuf>,
}

/// The coordinator writes its home and the memory folder (§7.1, §9); its git reads go to the
/// Hive's worktrees and folders. Workers get theirs in build step 4.
pub fn scope_for(hive: &Hive, role: &str, paths: &HivePaths) -> GateScope {
    let mut git_roots = vec![paths.worktrees(), paths.coordinator_home()];
    git_roots.extend(hive.folders.iter().map(|f| PathBuf::from(&f.path)));
    GateScope {
        role: role.to_string(),
        write_roots: vec![paths.coordinator_home(), paths.memory()],
        git_roots,
    }
}

const MAX_DEPTH: usize = 8;

pub fn gate_decision(tool: &str, input: &Value, cwd: Option<&Path>, scope: &GateScope) -> GateDecision {
    let verdict = match tool {
        "Bash" | "PowerShell" | "Monitor" => match input.get("command").and_then(Value::as_str) {
            Some(cmd) => check_command(cmd, cwd, scope, 0),
            None => Err("maiTerm couldn't read this command, so it is blocked.".to_string()),
        },
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
    if scope.write_roots.iter().filter_map(|r| real_path(r)).any(|r| real.starts_with(&r)) {
        Ok(())
    } else {
        Err(refuse())
    }
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

fn check_segment(words: &[String], cwd: Option<&Path>, scope: &GateScope, depth: usize) -> Result<(), String> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if KEYWORDS.contains(&w) || is_assignment(w) {
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

fn check_git(args: &[String], cwd: Option<&Path>, scope: &GateScope) -> Result<(), String> {
    let mut base: Option<PathBuf> = cwd.map(Path::to_path_buf);
    let check_path = |base: &Option<PathBuf>, p: &str, flag: &str| -> Result<PathBuf, String> {
        let real = resolve(base.as_deref(), p);
        match real {
            Some(r) if scope.git_roots.iter().filter_map(|g| real_path(g)).any(|g| r.starts_with(&g)) => Ok(r),
            _ => Err(format!("git {flag} {p} points outside this agent's folders, so it is blocked.")),
        }
    };
    let alias_refusal = |v: &str| {
        v.to_ascii_lowercase()
            .starts_with("alias.")
            .then(|| "git -c alias.… is refused in a Hive: an alias can rename push.".to_string())
    };
    let mut j = 0;
    while j < args.len() {
        let a = args[j].as_str();
        match a {
            "-C" => {
                let p = args.get(j + 1).ok_or("git -C with no folder")?;
                base = Some(check_path(&base, p, "-C")?);
                j += 2;
            }
            "-c" => {
                if let Some(r) = args.get(j + 1).and_then(|v| alias_refusal(v)) {
                    return Err(r);
                }
                j += 2;
            }
            "--git-dir" | "--work-tree" => {
                let p = args.get(j + 1).ok_or("a git option with no folder")?;
                check_path(&base, p, a)?;
                j += 2;
            }
            "--namespace" | "--super-prefix" | "--config-env" => j += 2,
            _ if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => {
                let (flag, p) = a.split_once('=').unwrap_or((a, ""));
                check_path(&base, p, flag)?;
                j += 1;
            }
            _ if a.starts_with("--config-env=") => {
                if let Some(r) = alias_refusal(&a["--config-env=".len()..]) {
                    return Err(r);
                }
                j += 1;
            }
            _ if a.starts_with('-') => j += 1,
            "push" | "send-pack" => {
                return Err("No agent in a Hive pushes: landed work is pushed by maiTerm itself, and only when the Hive allows it.".to_string());
            }
            "remote" => return Err("Hive agents can't use git remote: remotes are maiTerm's to manage.".to_string()),
            _ => return Ok(()),
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
    let decision = gate_decision(tool, &input, cwd.as_deref(), &scope_for(&hive, &role, &paths));
    if let GateDecision::Deny(reason) = &decision {
        log::info!("hive {}: gate refused {tool} in tab {tab_id}: {reason}", hive.id);
        log_refusal(&paths, tab_id, tool, &describe(&input), reason);
    }
    decision
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

    struct Fx {
        base: PathBuf,
        paths: HivePaths,
        scope: GateScope,
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
        let mut hive = crate::hive::launch::tests::sample_hive();
        hive.folders.truncate(1);
        hive.folders[0].path = base.join("repo/web").to_string_lossy().to_string();
        let scope = scope_for(&hive, crate::hive::ROLE_COORDINATOR, &paths);
        Fx { base, paths, scope }
    }

    fn bash(f: &Fx, cmd: &str) -> GateDecision {
        gate_decision("Bash", &json!({ "command": cmd }), Some(&f.paths.coordinator_home()), &f.scope)
    }

    /// Spike 9's table: what a hook like this caught, and the forms around it.
    #[test]
    fn pushes_and_their_wrappers_are_refused() {
        let f = fx();
        let wt = f.paths.coordinator_worktree("web");
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
            "git -c alias.p=push p".into(),
            "gh pr merge 3 --squash".into(),
            "gh -R o/r pr merge 3".into(),
        ];
        for cmd in &refused {
            assert!(matches!(bash(&f, cmd), GateDecision::Deny(_)), "should refuse: {cmd}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn reads_and_reviews_inside_the_folders_pass() {
        let f = fx();
        let wt = f.paths.coordinator_worktree("web");
        let allowed = [
            "git diff".to_string(),
            "git log --oneline -5".into(),
            "git log --grep push".into(),
            "git show HEAD:README.md".into(),
            format!("git -C '{}' diff HEAD~1", wt.display()),
            format!("git -C {} log", f.base.join("repo/web").display()),
            "echo push".into(),
            "ls -la".into(),
            "git status # git push later".into(),
        ];
        for cmd in &allowed {
            assert_eq!(bash(&f, cmd), GateDecision::Allow, "should allow: {cmd}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn git_pointed_outside_the_folders_is_refused() {
        let f = fx();
        let out = f.base.join("outside");
        for cmd in [
            format!("git -C {} diff", out.display()),
            format!("git --git-dir={}/.git log", out.display()),
            format!("git --work-tree {} status", out.display()),
            "git -C ../../.. log".to_string(),
            "git -C /etc log".into(),
        ] {
            assert!(matches!(bash(&f, &cmd), GateDecision::Deny(r) if r.contains("outside")), "{cmd}");
        }
        let _ = fs::remove_dir_all(&f.base);
    }

    /// What spike 9 saw get past a hook like this. Not claimed: the sandbox and the unreachable
    /// remotes are what hold these.
    #[test]
    fn the_known_bypasses_are_not_claimed() {
        let f = fx();
        assert_eq!(bash(&f, "c=pu; git ${c}sh"), GateDecision::Allow);
        assert_eq!(bash(&f, "echo Z2l0IHB1c2g= | base64 -d | sh"), GateDecision::Allow);
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn writes_land_only_in_home_and_memory_by_real_path() {
        let f = fx();
        let home = f.paths.coordinator_home();
        let w = |p: &Path| gate_decision("Write", &json!({ "file_path": p }), Some(&home), &f.scope);
        assert_eq!(w(&home.join("plan.md")), GateDecision::Allow);
        assert_eq!(w(&home.join("new/dir/plan.md")), GateDecision::Allow, "a folder that doesn't exist yet");
        assert_eq!(w(&f.paths.memory().join("decision.md")), GateDecision::Allow);
        assert_eq!(gate_decision("Edit", &json!({ "file_path": "notes.md" }), Some(&home), &f.scope), GateDecision::Allow, "relative to cwd");
        assert!(matches!(w(&f.base.join("outside/x")), GateDecision::Deny(_)));
        assert!(matches!(w(&f.paths.coordinator_worktree("web").join("src.rs")), GateDecision::Deny(_)), "read copies are read-only");
        assert!(matches!(w(&home.join("../../../outside/x")), GateDecision::Deny(_)), "climbing out by ..");
        assert!(matches!(w(&home.join("missing/../../outside/x")), GateDecision::Deny(_)), "climbing out past a missing folder");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(f.base.join("outside"), home.join("escape")).unwrap();
            assert!(matches!(w(&home.join("escape/x")), GateDecision::Deny(_)), "a symlink out of home");
            std::os::unix::fs::symlink(f.base.join("outside/nothing-here"), home.join("dangling")).unwrap();
            assert!(matches!(w(&home.join("dangling")), GateDecision::Deny(_)), "a dangling symlink can't be checked");
        }
        assert!(matches!(gate_decision("Write", &json!({}), Some(&home), &f.scope), GateDecision::Deny(_)));
        assert_eq!(gate_decision("Read", &json!({ "file_path": "/etc/passwd" }), Some(&home), &f.scope), GateDecision::Allow, "not the gate's");
        let _ = fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_event_the_gate_cannot_place_is_refused() {
        let f = fx();
        let hive = crate::hive::launch::tests::sample_hive();
        let agent = || Some((hive.clone(), crate::hive::ROLE_COORDINATOR.to_string(), HivePaths::new(f.paths.root.clone())));
        assert!(matches!(decide_event("t", None, r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#), GateDecision::Deny(_)), "not a Hive tab");
        assert!(matches!(decide_event("t", agent(), "{not json"), GateDecision::Deny(_)));
        let home = f.paths.coordinator_home().to_string_lossy().to_string();
        let ok = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git diff"}, "cwd": home}).to_string();
        assert_eq!(decide_event("t", agent(), &ok), GateDecision::Allow);
        let push = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "git push"}, "cwd": home}).to_string();
        assert!(matches!(decide_event("t", agent(), &push), GateDecision::Deny(_)));
        assert!(fs::read_to_string(f.paths.logs().join("gate.log")).unwrap().contains("git push"), "the refusal is on the record");
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
