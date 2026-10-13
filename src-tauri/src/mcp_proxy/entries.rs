//! Writing each proxied server into the agents' own configs (docs/mcp-proxy.md §3, §6).
//!
//! One entry per server, under the server's name (`<id>-dev` from a dev build, as `maiterm-dev`),
//! pointing at `/mcp/x/<id>`. Every entry carries a marker header `x-maiterm-proxy`, which is the
//! only thing that makes an entry ours: an entry without it under the same name is the human's
//! native install, and is never overwritten (a *collision*, shown in Preferences). An entry with
//! it that no enabled server wants any more is removed.
//!
//! - **Claude:** `mcpServers` in `~/.claude.json`, pushed into every managed account root the
//!   way maiTerm's own entry is.
//! - **Codex:** `[mcp_servers.<name>]` in `~/.codex/config.toml` (account roots symlink it), when
//!   the Codex integration is on.
//!
//! Written at startup before any tab spawns, re-asserted by the 30 s drift loop, rewritten when a
//! server is added, removed or switched, and removed at exit with maiTerm's own entry: the port
//! changes every launch, and an entry dialling a dead port is a failed server in every session.

use super::ProxyServer;
use crate::state::Preferences;
use serde_json::{json, Value};
use std::path::Path;

/// The header that marks an entry as maiTerm's, valued `<data slug>/<server id>`. The slug says
/// which maiTerm wrote it: dev and the installed app share these files, and each must leave the
/// other's entries alone.
pub const MARKER: &str = "x-maiterm-proxy";

fn marker_value(id: &str) -> String {
    format!("{}/{id}", crate::state::persistence::app_data_slug())
}

/// The server id in a marker this maiTerm wrote; `None` for another flavor's.
fn ours_id(marker: &str) -> Option<&str> {
    marker.strip_prefix(crate::state::persistence::app_data_slug())?.strip_prefix('/')
}

/// Our writes to the two files, one at a time.
static WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The entry name a server gets in the runtimes' configs.
pub fn entry_name(id: &str) -> String {
    if cfg!(debug_assertions) {
        format!("{id}-dev")
    } else {
        id.to_string()
    }
}

fn claude_entry(port: u16, auth: &str, id: &str) -> Value {
    json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/mcp/x/{}", entry_name(id)),
        "headers": {
            "x-claude-code-ide-authorization": auth,
            "x-maiterm-tab": "${MAITERM_TAB_ID}",
            MARKER: marker_value(id),
        }
    })
}

/// Is this `mcpServers` value one of ours, and if so for which server?
fn claude_marker(entry: &Value) -> Option<&str> {
    entry.get("headers")?.get(MARKER)?.as_str()
}

/// The server id of an entry THIS maiTerm wrote.
fn claude_ours(entry: &Value) -> Option<&str> {
    ours_id(claude_marker(entry)?)
}

/// What applying the wanted set to one runtime's config found.
#[derive(Default, Debug, PartialEq)]
pub struct Applied {
    pub changed: bool,
    /// Server ids whose name is held by a native entry, so they are not written.
    pub collisions: Vec<String>,
}

/// Make `mcpServers` hold exactly our wanted entries, leaving every native one alone.
fn apply_claude(root: &mut Value, wanted: &[(String, Value)]) -> Applied {
    let mut out = Applied::default();
    let Some(obj) = root.as_object_mut() else { return out };
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else { return out };
    let stale: Vec<String> = servers
        .iter()
        .filter(|(name, e)| claude_ours(e).is_some() && !wanted.iter().any(|(n, _)| n == *name))
        .map(|(name, _)| name.clone())
        .collect();
    for name in stale {
        servers.remove(&name);
        out.changed = true;
    }
    for (name, entry) in wanted {
        match servers.get(name) {
            Some(existing) if claude_ours(existing).is_none() => {
                out.collisions.push(claude_ours(entry).unwrap_or_default().to_string());
            }
            Some(existing) if existing == entry => {}
            _ => {
                servers.insert(name.clone(), entry.clone());
                out.changed = true;
            }
        }
    }
    out
}

fn codex_marker(entry: &toml_edit::Item) -> Option<String> {
    let headers = entry.as_table_like()?.get("http_headers")?;
    headers.as_table_like()?.get(MARKER)?.as_str().map(str::to_string)
}

fn codex_table(port: u16, auth: &str, id: &str) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    t.insert("url", toml_edit::value(format!("http://127.0.0.1:{port}/mcp/x/{}", entry_name(id))));
    let mut headers = toml_edit::InlineTable::new();
    headers.insert("x-maiterm-authorization", auth.into());
    headers.insert(MARKER, marker_value(id).into());
    t.insert("http_headers", toml_edit::value(headers));
    let mut env_headers = toml_edit::InlineTable::new();
    env_headers.insert("x-maiterm-tab", "MAITERM_TAB_ID".into());
    t.insert("env_http_headers", toml_edit::value(env_headers));
    t
}

/// The fields of an entry that matter, for comparing ours with what is on disk.
fn codex_shape(item: &toml_edit::Item) -> Option<(String, String, String)> {
    let t = item.as_table_like()?;
    let url = t.get("url")?.as_str()?.to_string();
    let h = t.get("http_headers")?.as_table_like()?;
    let auth = h.get("x-maiterm-authorization")?.as_str()?.to_string();
    let tab = t.get("env_http_headers")?.as_table_like()?.get("x-maiterm-tab")?.as_str()?.to_string();
    Some((url, auth, tab))
}

fn apply_codex(doc: &mut toml_edit::DocumentMut, wanted: &[(String, String, toml_edit::Table)]) -> Applied {
    let mut out = Applied::default();
    let servers = match doc.entry("mcp_servers").or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new())).as_table_like_mut() {
        Some(t) => t,
        None => return out, // not a table: the user's, left alone (codex.rs refuses the same way)
    };
    let stale: Vec<String> = servers
        .iter()
        .filter(|(name, e)| codex_marker(e).is_some_and(|m| ours_id(&m).is_some()) && !wanted.iter().any(|(n, _, _)| n == name))
        .map(|(name, _)| name.to_string())
        .collect();
    for name in stale {
        servers.remove(&name);
        out.changed = true;
    }
    for (name, id, table) in wanted {
        let item = toml_edit::Item::Table(table.clone());
        match servers.get_mut(name) {
            Some(existing) if !codex_marker(existing).is_some_and(|m| ours_id(&m).is_some()) => out.collisions.push(id.clone()),
            Some(existing) if codex_shape(existing) == codex_shape(&item) => {}
            // Ours but stale (the port changes every launch): update only our keys, so a key the
            // human added to the table (`disabled_tools`, `tool_timeout_sec`) survives.
            Some(existing) => {
                if let Some(t) = existing.as_table_like_mut() {
                    for (k, v) in table.iter() {
                        t.insert(k, v.clone());
                    }
                    out.changed = true;
                }
            }
            None => {
                servers.insert(name, item);
                out.changed = true;
            }
        }
    }
    out
}

/// The servers whose entries should exist now.
fn wanted_servers() -> Vec<ProxyServer> {
    super::all().into_iter().filter(|s| s.enabled).collect()
}

/// Bring both runtimes' configs in line with the enabled servers. Cheap when nothing changed:
/// files are compared before anything is written. Returns the collisions found, by runtime.
pub fn sync(port: u16, auth: &str, prefs: &Preferences) -> Collisions {
    super::ensure_loaded();
    let _g = WRITE.lock();
    // Read what is wanted only once the lock is held: a pass that waited behind another must not
    // write a snapshot from before a server was removed or a runtime switched off.
    let servers = wanted_servers();
    let live_prefs = super::APP.get().map(|(_, st)| st.app_data.read().preferences.clone());
    let prefs = live_prefs.as_ref().unwrap_or(prefs);
    let mut found = Collisions::default();

    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        let wanted: Vec<(String, Value)> = if prefs.claude_ide {
            servers.iter().map(|s| (entry_name(&s.id), claude_entry(port, auth, &s.id))).collect()
        } else {
            vec![]
        };
        match edit_json(&path, |root| apply_claude(root, &wanted)) {
            Ok(a) => {
                if a.changed {
                    log::info!("[mcp-proxy] wrote {} entr{} into ~/.claude.json", wanted.len(), if wanted.len() == 1 { "y" } else { "ies" });
                }
                // A managed account root keeps its own copy of the file, and a running `claude`
                // under it saves its in-memory copy back over ours. So the roots are checked on
                // every pass, not only when ~/.claude.json changed.
                if a.changed || roots_drifted(&wanted, &a.collisions) {
                    crate::claude_code::lockfile::propagate_to_account_roots();
                }
                found.claude = a.collisions;
            }
            Err(e) => log::warn!("[mcp-proxy] ~/.claude.json: {e}"),
        }
    }

    if let Some(home) = dirs::home_dir() {
        let path = home.join(".codex").join("config.toml");
        let wanted: Vec<(String, String, toml_edit::Table)> = if prefs.codex_ide {
            servers.iter().map(|s| (entry_name(&s.id), s.id.clone(), codex_table(port, auth, &s.id))).collect()
        } else {
            vec![]
        };
        // No Codex here and nothing to write: don't create its config.
        if path.exists() || !wanted.is_empty() && home.join(".codex").exists() {
            match crate::claude_code::codex::read_document(&path) {
                Ok(mut doc) => {
                    let a = apply_codex(&mut doc, &wanted);
                    if a.changed {
                        match write_toml(&path, &doc.to_string()) {
                            Ok(()) => log::info!("[mcp-proxy] wrote {} entr{} into ~/.codex/config.toml", wanted.len(), if wanted.len() == 1 { "y" } else { "ies" }),
                            Err(e) => log::warn!("[mcp-proxy] ~/.codex/config.toml: {e}"),
                        }
                    }
                    found.codex = a.collisions;
                }
                Err(e) => log::warn!("[mcp-proxy] ~/.codex/config.toml: {e}"),
            }
        }
    }
    *super::COLLISIONS.lock() = found.clone();
    found
}

/// What the ssh-host setup writes into the remote account's `~/.claude.json` (docs/mcp-proxy.md
/// §6): `set`, entries by name.
///
/// Every value is a placeholder the remote agent expands from its own shell, exactly as for the
/// remote `maiterm` entry: the account may be shared by several of this human's tabs, by other
/// people's maiTerms, and by both flavors, so the bytes must be the same from every writer.
/// That is also why the remote side only ever ADDS, and never deletes: an entry marked by
/// maiTerm can't say WHICH maiTerm wants it (the marker is the same for all of a flavor), so
/// deleting the one this maiTerm removed would take it from a teammate still using it. One this
/// maiTerm no longer serves answers as an empty server (`not_served`) instead. A native entry
/// under a name is never overwritten there either (the setup script checks the marker).
pub fn remote() -> Value {
    super::ensure_loaded();
    let servers = super::all();
    let set: serde_json::Map<String, Value> = servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| {
            let entry = json!({
                "type": "http",
                "url": format!("http://127.0.0.1:${{MAITERM_PORT}}/mcp/x/{}", entry_name(&s.id)),
                "headers": {
                    "x-claude-code-ide-authorization": "${MAITERM_AUTH}",
                    "x-maiterm-tab": "${MAITERM_TAB_ID}",
                    MARKER: marker_value(&s.id),
                }
            });
            (entry_name(&s.id), entry)
        })
        .collect();
    json!({ "set": set })
}

/// Names of the human's own (unmarked) entries in Claude's and Codex's configs here: a proxied
/// server under one of them would not be written there (a collision), so an agent's proposal
/// must pick another name.
pub fn native_names() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        if let Some(root) = std::fs::read_to_string(&path).ok().and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
            if let Some(m) = root.get("mcpServers").and_then(Value::as_object) {
                out.extend(m.iter().filter(|(_, e)| claude_ours(e).is_none()).map(|(n, _)| n.clone()));
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        if let Ok(doc) = crate::claude_code::codex::read_document(&home.join(".codex").join("config.toml")) {
            if let Some(servers) = doc.get("mcp_servers").and_then(|s| s.as_table_like()) {
                out.extend(servers.iter().filter(|(_, e)| !codex_marker(e).is_some_and(|m| ours_id(&m).is_some())).map(|(n, _)| n.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Does any managed Claude account root lack an entry we wrote, or hold one we removed?
/// Read-only; the push itself is `propagate_to_account_roots`.
fn roots_drifted(wanted: &[(String, Value)], collisions: &[String]) -> bool {
    let Some(dir) = crate::accounts::accounts_dir().map(|d| d.join(crate::accounts::Runtime::Claude.slug())) else { return false };
    let Ok(read) = std::fs::read_dir(&dir) else { return false };
    let written: Vec<&(String, Value)> = wanted
        .iter()
        .filter(|(_, e)| !collisions.iter().any(|c| Some(c.as_str()) == claude_ours(e)))
        .collect();
    for root in read.flatten() {
        let Some(servers) = std::fs::read_to_string(root.path().join(".claude.json"))
            .ok()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|v| v.get("mcpServers").cloned())
        else {
            continue;
        };
        let missing = written.iter().any(|(n, e)| servers.get(n) != Some(e));
        let extra = servers
            .as_object()
            .is_some_and(|m| m.iter().any(|(n, e)| claude_ours(e).is_some() && !written.iter().any(|(w, _)| w == n)));
        if missing || extra {
            return true;
        }
    }
    false
}

/// Remove every entry of ours (app exit).
pub fn remove_all() {
    let _g = WRITE.lock();
    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        if path.exists() {
            if let Ok(a) = edit_json(&path, |root| apply_claude(root, &[])) {
                if a.changed {
                    crate::claude_code::lockfile::propagate_to_account_roots();
                }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".codex").join("config.toml");
        if path.exists() {
            if let Ok(mut doc) = crate::claude_code::codex::read_document(&path) {
                if apply_codex(&mut doc, &[]).changed {
                    let _ = write_toml(&path, &doc.to_string());
                }
            }
        }
    }
}

/// Collisions by runtime: server ids whose name a native entry holds.
#[derive(Clone, Default, Debug)]
pub struct Collisions {
    pub claude: Vec<String>,
    pub codex: Vec<String>,
}

/// Read, change and write back a JSON config. A file that exists and does not parse is never
/// written: it may be the `claude` CLI's half-written save, and replacing it would lose the
/// human's settings.
fn edit_json(path: &Path, f: impl FnOnce(&mut Value) -> Applied) -> Result<Applied, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => Some(r),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read: {e}")),
    };
    let mut root: Value = match &raw {
        Some(r) => serde_json::from_str(r).map_err(|e| format!("not valid JSON, left alone: {e}"))?,
        None => json!({}),
    };
    let applied = f(&mut root);
    if applied.changed {
        // `mcpServers` emptied by us goes away, as maiTerm's own removal does.
        if root.get("mcpServers").and_then(Value::as_object).is_some_and(|m| m.is_empty()) {
            root.as_object_mut().map(|o| o.remove("mcpServers"));
        }
        let rendered = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.maiterm-proxy-tmp");
        std::fs::write(&tmp, rendered).map_err(|e| format!("cannot write: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write: {e}")
        })?;
    }
    Ok(applied)
}

/// Atomic write of `config.toml` through a temp file of our own: the Codex registrar writes the
/// same file through `config.maiterm-tmp`, and two writers sharing one temp name can rename each
/// other's half-written file into place.
fn write_toml(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("maiterm-proxy-tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|e| format!("cannot write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write: {e}")
    })
}

// ─── Native installs of the same server ───────────────────────────────────

fn same_url(a: &str, b: &str) -> bool {
    let norm = |u: &str| u.trim().trim_end_matches('/').to_ascii_lowercase();
    norm(a) == norm(b)
}

/// Native entries elsewhere that reach the same URL, as "Claude: name", "Claude (project
/// ~/code/x): name" or "Codex: name". Two copies double every tool in context (§3).
pub fn native_duplicates(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let home = dirs::home_dir();
    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        if let Some(root) = std::fs::read_to_string(&path).ok().and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
            out.extend(claude_duplicates(&root, url, home.as_deref()));
        }
    }
    if let Some(home) = &home {
        let path = home.join(".codex").join("config.toml");
        if let Ok(doc) = crate::claude_code::codex::read_document(&path) {
            out.extend(codex_duplicates(&doc, url));
        }
    }
    out
}

fn claude_duplicates(root: &Value, url: &str, home: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    let scan = |servers: Option<&Value>, label: &str, out: &mut Vec<String>| {
        for (name, e) in servers.and_then(Value::as_object).into_iter().flatten() {
            if claude_marker(e).is_none() && e.get("url").and_then(Value::as_str).is_some_and(|u| same_url(u, url)) {
                out.push(format!("{label}: {name}"));
            }
        }
    };
    scan(root.get("mcpServers"), "Claude", &mut out);
    for (dir, p) in root.get("projects").and_then(Value::as_object).into_iter().flatten() {
        let shown = match home.and_then(|h| Path::new(dir).strip_prefix(h).ok()) {
            Some(rel) => format!("~/{}", rel.display()),
            None => dir.clone(),
        };
        scan(p.get("mcpServers"), &format!("Claude (project {shown})"), &mut out);
    }
    out
}

fn codex_duplicates(doc: &toml_edit::DocumentMut, url: &str) -> Vec<String> {
    let Some(servers) = doc.get("mcp_servers").and_then(|s| s.as_table_like()) else { return vec![] };
    servers
        .iter()
        .filter(|(_, e)| codex_marker(e).is_none())
        .filter(|(_, e)| e.as_table_like().and_then(|t| t.get("url")).and_then(|u| u.as_str()).is_some_and(|u| same_url(u, url)))
        .map(|(name, _)| format!("Codex: {name}"))
        .collect()
}

/// Remove the human's native entries for this URL (and a native entry holding the server's
/// name), on their click of **Move to maiTerm**. Returns what was removed.
pub fn adopt_native(id: &str, url: &str) -> Result<Vec<String>, String> {
    let name = entry_name(id);
    let _g = WRITE.lock();
    let mut removed = Vec::new();
    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        if path.exists() {
            let mut gone = Vec::new();
            edit_json(&path, |root| {
                let mut a = Applied::default();
                let mut strip = |servers: Option<&mut Value>, label: &str| {
                    let Some(m) = servers.and_then(Value::as_object_mut) else { return };
                    let doomed: Vec<String> = m
                        .iter()
                        .filter(|(n, e)| claude_marker(e).is_none() && (**n == name || e.get("url").and_then(Value::as_str).is_some_and(|u| same_url(u, url))))
                        .map(|(n, _)| n.clone())
                        .collect();
                    for n in doomed {
                        m.remove(&n);
                        gone.push(format!("{label}: {n}"));
                        a.changed = true;
                    }
                };
                strip(root.get_mut("mcpServers"), "Claude");
                if let Some(projects) = root.get_mut("projects").and_then(Value::as_object_mut) {
                    for (dir, p) in projects.iter_mut() {
                        strip(p.get_mut("mcpServers"), &format!("Claude (project {dir})"));
                    }
                }
                a
            })?;
            if !gone.is_empty() {
                crate::claude_code::lockfile::propagate_to_account_roots();
            }
            removed.extend(gone);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".codex").join("config.toml");
        if path.exists() {
            let mut doc = crate::claude_code::codex::read_document(&path)?;
            let mut gone = Vec::new();
            if let Some(servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_like_mut()) {
                let doomed: Vec<String> = servers
                    .iter()
                    .filter(|(n, e)| {
                        codex_marker(e).is_none()
                            && (*n == name || e.as_table_like().and_then(|t| t.get("url")).and_then(|u| u.as_str()).is_some_and(|u| same_url(u, url)))
                    })
                    .map(|(n, _)| n.to_string())
                    .collect();
                for n in doomed {
                    servers.remove(&n);
                    gone.push(format!("Codex: {n}"));
                }
            }
            if !gone.is_empty() {
                write_toml(&path, &doc.to_string())?;
            }
            removed.extend(gone);
        }
    }
    if !removed.is_empty() {
        log::info!("[mcp-proxy] {id}: moved to maiTerm, removed native {}", removed.join(", "));
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(id: &str) -> (String, Value) {
        (id.to_string(), claude_entry(9, "tok", id))
    }

    #[test]
    fn claude_native_entries_are_never_touched() {
        let mut root = json!({ "mcpServers": { "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }, "other": { "command": "x" } } });
        let a = apply_claude(&mut root, &[ours("linear"), ours("notion")]);
        assert_eq!(a.collisions, vec!["linear"]);
        assert!(a.changed);
        assert_eq!(root["mcpServers"]["linear"]["url"], "https://mcp.linear.app/mcp");
        assert_eq!(root["mcpServers"]["other"]["command"], "x");
        assert_eq!(claude_ours(&root["mcpServers"]["notion"]), Some("notion"));
    }

    #[test]
    fn claude_our_unwanted_entries_go_and_a_rerun_writes_nothing() {
        let mut root = json!({});
        apply_claude(&mut root, &[ours("a"), ours("b")]);
        let again = apply_claude(&mut root, &[ours("a"), ours("b")]);
        assert!(!again.changed, "an unchanged set must not rewrite the file");
        let a = apply_claude(&mut root, &[ours("a")]);
        assert!(a.changed);
        assert!(root["mcpServers"].get("b").is_none());
        assert!(root["mcpServers"].get("a").is_some());
    }

    #[test]
    fn the_other_maiterms_entries_are_left_alone() {
        // The installed app's entry, seen by a dev build (or the reverse): marked, but not ours.
        let theirs = json!({ "type": "http", "url": "http://127.0.0.1:1/mcp/x/linear", "headers": { MARKER: "com.example.other/linear" } });
        let mut root = json!({ "mcpServers": { "linear": theirs.clone() } });
        let a = apply_claude(&mut root, &[]);
        assert!(!a.changed, "another flavor's entry must not be swept");
        let a = apply_claude(&mut root, &[ours("linear")]);
        assert_eq!(a.collisions, vec!["linear"], "nor overwritten");
        assert_eq!(root["mcpServers"]["linear"], theirs);
    }

    #[test]
    fn codex_native_tables_are_never_touched_and_ours_converge() {
        let mut doc: toml_edit::DocumentMut = "[mcp_servers.linear]\nurl = \"https://mcp.linear.app/mcp\"\n\n[mcp_servers.maiterm]\nurl = \"http://127.0.0.1:1/mcp\"\n".parse().unwrap();
        let wanted = vec![
            ("linear".to_string(), "linear".to_string(), codex_table(9, "tok", "linear")),
            ("notion".to_string(), "notion".to_string(), codex_table(9, "tok", "notion")),
        ];
        let a = apply_codex(&mut doc, &wanted);
        assert_eq!(a.collisions, vec!["linear"]);
        assert!(a.changed);
        let text = doc.to_string();
        assert!(text.contains("https://mcp.linear.app/mcp"));
        assert!(text.contains("[mcp_servers.notion]"), "{text}");
        assert!(text.contains("[mcp_servers.maiterm]"));
        assert!(!apply_codex(&mut doc, &wanted).changed);
        assert!(apply_codex(&mut doc, &[]).changed);
        assert!(!doc.to_string().contains("notion"));
        assert!(doc.to_string().contains("[mcp_servers.linear]"));
    }

    #[test]
    fn codex_keys_the_human_added_to_our_entry_survive_a_new_port() {
        let mut doc = toml_edit::DocumentMut::new();
        apply_codex(&mut doc, &[("linear".into(), "linear".into(), codex_table(1, "tok", "linear"))]);
        doc["mcp_servers"]["linear"]["disabled_tools"] = toml_edit::value(toml_edit::Array::from_iter(["delete_issue"]));
        let a = apply_codex(&mut doc, &[("linear".into(), "linear".into(), codex_table(2, "tok", "linear"))]);
        assert!(a.changed);
        let text = doc.to_string();
        assert!(text.contains("127.0.0.1:2/mcp/x/linear"), "{text}");
        assert!(text.contains("disabled_tools"), "{text}");
    }

    #[test]
    fn duplicates_are_found_by_url_wherever_they_are() {
        let root = json!({
            "mcpServers": { "lin": { "url": "https://mcp.linear.app/mcp/" }, "ours": claude_entry(9, "t", "linear") },
            "projects": { "/Users/x/code/a": { "mcpServers": { "linear": { "url": "https://MCP.linear.app/mcp" } } } }
        });
        let d = claude_duplicates(&root, "https://mcp.linear.app/mcp", Some(Path::new("/Users/x")));
        assert_eq!(d, vec!["Claude: lin", "Claude (project ~/code/a): linear"]);
    }
}
