use super::*;
use crate::hive::create::ProxyRef;
use crate::mcp_proxy::entries::entry_name;

fn tool(id: &str, kind: &str, label: &str, host: Option<&str>, proxy: Option<ProxyRef>) -> DetectedTool {
    DetectedTool { id: id.to_string(), kind: kind.to_string(), label: label.to_string(), host: host.map(str::to_string), pack: None, proxy }
}

fn cli(bin: &str) -> DetectedTool {
    tool(&format!("cli:{bin}"), "cli", bin, None, None)
}

fn cover(tool_id: &str, provider: &str, state: &str) -> Cover {
    Cover { tool_id: tool_id.to_string(), provider: provider.to_string(), state: state.to_string() }
}

fn server(id: &str, url: &str, status: &str) -> ProxyInput {
    ProxyInput { id: id.to_string(), url: url.to_string(), enabled: true, workspaces: Vec::new(), status: status.to_string() }
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hive-media-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn state_of(hits: &[Hit], tool_id: &str, provider: &str) -> Option<String> {
    hits.iter().find(|h| h.cover.tool_id == tool_id && h.cover.provider == provider).map(|h| h.cover.state.clone())
}

// ─── Programs ───────────────────────────────────────────────────────────────

#[test]
fn programs_cover_what_the_catalogue_says_and_brew_covers_nothing() {
    let hits = cli_hits(&[cli("ffmpeg"), cli("ffprobe"), cli("whisper-cli"), cli("blender"), cli("brew"), cli("git")]);
    assert_eq!(state_of(&hits, "cli:ffmpeg", "homebrew-kit").as_deref(), Some(READY));
    assert_eq!(state_of(&hits, "cli:ffprobe", "homebrew-kit").as_deref(), Some(READY));
    let whisper = hits.iter().find(|h| h.cover.tool_id == "cli:whisper-cli").unwrap();
    assert_eq!(whisper.capabilities, vec!["transcribe"]);
    let blender = hits.iter().find(|h| h.cover.tool_id == "cli:blender").unwrap();
    assert_eq!((blender.cover.provider.as_str(), blender.capabilities.clone()), ("blender", vec!["3d".to_string()]));
    assert!(hits.iter().all(|h| h.cover.tool_id != "cli:brew" && h.cover.tool_id != "cli:git"), "brew and git are not media tools");
    // Only a `cli` kind: an MCP server that happens to be named ffmpeg is not the program.
    assert!(cli_hits(&[tool("mcp:ffmpeg", "mcp", "ffmpeg", None, None)]).is_empty());
}

#[test]
fn detection_looks_for_the_three_new_programs() {
    for bin in ["whisper-cli", "ffprobe", "brew"] {
        assert!(crate::hive::create::DETECTED_CLIS.contains(&bin), "{bin}");
    }
}

// ─── MCP proxy ──────────────────────────────────────────────────────────────

#[test]
fn a_proxy_servers_state_follows_its_status_and_its_scope() {
    let fal = "https://mcp.fal.ai/mcp-relay";
    for (status, want) in [("connected", READY), ("needs_sign_in", NEEDS_SIGN_IN), ("unreachable", UNREACHABLE), ("error", UNREACHABLE), ("disabled", UNREACHABLE), ("connecting", INSTALLED)] {
        assert_eq!(proxy_state(&server("fal", fal, status), None), want, "{status}");
    }
    let mut off = server("fal", fal, "connected");
    off.enabled = false;
    assert_eq!(proxy_state(&off, None), UNREACHABLE, "a server the human turned off");
    // Scoped to workspace w1: in scope there, never ready elsewhere, whatever its health.
    let mut scoped = server("fal", fal, "connected");
    scoped.workspaces = vec!["w1".to_string()];
    assert_eq!(proxy_state(&scoped, Some("w1")), READY);
    assert_eq!(proxy_state(&scoped, Some("w2")), UNREACHABLE, "scoped away from this Hive's workspace");
    assert_eq!(proxy_state(&scoped, None), READY, "Preferences has no workspace to be scoped away from");
    scoped.status = "needs_sign_in".to_string();
    assert_eq!(proxy_state(&scoped, Some("w2")), UNREACHABLE, "scope outranks the sign-in");
    assert_eq!(proxy_state(&scoped, Some("w1")), NEEDS_SIGN_IN);
    // Offered everywhere (empty scope) is in scope everywhere.
    assert_eq!(proxy_state(&server("fal", fal, "connected"), Some("any")), READY);
}

#[test]
fn a_proxy_server_is_recognised_by_its_upstream_never_its_name() {
    let hits = proxy_hits(
        &[server("video", "https://mcp.fal.ai/mcp-relay", "connected"), server("fal", "https://evil.example.com/mcp", "connected"), server("linear", "https://mcp.linear.app/mcp", "connected")],
        None,
    );
    // "video" dials fal: fal, for everything fal covers, under the entry's name.
    let video = hits.iter().find(|h| h.cover.tool_id == format!("mcp:{}", entry_name("video"))).unwrap();
    assert_eq!(video.cover.provider, "fal");
    assert_eq!(video.cover.state, READY);
    assert!(video.capabilities.iter().any(|c| c == "video") && video.capabilities.iter().any(|c| c == "3d"));
    // "fal" that points elsewhere, and a server that is no generator, are nothing.
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_proxy_server_scoped_away_is_unreachable_in_the_row_never_ready() {
    let mut fal = server("fal", "https://mcp.fal.ai/mcp-relay", "connected");
    fal.workspaces = vec!["other".to_string()];
    let here = rows(&proxy_hits(std::slice::from_ref(&fal), Some("mine")), &[]);
    let video = here.iter().find(|r| r.capability == "video").unwrap();
    assert_eq!(video.covered_by, vec![cover(&format!("mcp:{}", entry_name("fal")), "fal", UNREACHABLE)]);
    assert_eq!(video.recommended.as_deref(), Some("fal"), "not ready, so fal is still what to set up for this Hive");
    let there = rows(&proxy_hits(&[fal], Some("other")), &[]);
    let video = there.iter().find(|r| r.capability == "video").unwrap();
    assert_eq!(video.covered_by[0].state, READY);
    assert_eq!(video.recommended, None);
}

// ─── Native .claude.json servers ────────────────────────────────────────────

#[test]
fn a_native_server_is_recognised_by_host_then_name_and_is_installed_not_ready() {
    let hits = native_hits(&[
        tool("mcp:whatever", "mcp", "whatever", Some("mcp.higgsfield.ai"), None),
        tool("mcp:elevenlabs", "mcp", "elevenlabs", None, None),
        tool("mcp:github", "mcp", "github", Some("api.githubcopilot.com"), None),
        // A proxied entry is the registry's to report (`proxy_hits`), by its upstream.
        tool("mcp:fal-dev", "mcp", "fal-dev", Some("127.0.0.1:8765"), Some(ProxyRef { id: "fal".to_string(), upstream: Some("https://mcp.fal.ai/mcp-relay".to_string()) })),
        // Marked as ours but not provably so: reaches nothing we can name, whatever its name says.
        tool("mcp:runway", "mcp", "runway", None, Some(ProxyRef { id: String::new(), upstream: None })),
    ]);
    assert_eq!(state_of(&hits, "mcp:whatever", "higgsfield").as_deref(), Some(INSTALLED));
    assert_eq!(state_of(&hits, "mcp:elevenlabs", "elevenlabs").as_deref(), Some(INSTALLED));
    assert!(hits.iter().all(|h| h.cover.tool_id != "mcp:github" && h.cover.tool_id != "mcp:fal-dev" && h.cover.tool_id != "mcp:runway"), "{hits:?}");
}

// ─── Claude Code plugins ────────────────────────────────────────────────────

fn write_plugins(root: &Path, json: &str) {
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    std::fs::write(root.join("plugins").join("installed_plugins.json"), json).unwrap();
}

fn plugin_json(install: &Path, gone: &Path) -> String {
    format!(
        r#"{{ "version": 2, "plugins": {{
            "remotion@remotion": [ {{ "scope": "user", "installPath": {install:?}, "version": "1.0.0" }} ],
            "project-only@m": [ {{ "scope": "project", "projectPath": "/x", "installPath": {install:?} }} ],
            "local-only@m": [ {{ "scope": "local", "installPath": {install:?} }} ],
            "deleted@m": [ {{ "scope": "user", "installPath": {gone:?} }} ],
            "mixed@m": [ {{ "scope": "project", "installPath": {install:?} }}, {{ "scope": "user", "installPath": {install:?} }} ]
        }} }}"#
    )
}

#[test]
fn installed_plugins_reads_claude_codes_record() {
    let root = temp("plugins");
    let install = root.join("plugins").join("cache").join("remotion");
    std::fs::create_dir_all(&install).unwrap();
    write_plugins(&root, &plugin_json(&install, &root.join("nowhere")));
    let mut got = installed_plugins(&root);
    got.sort();
    assert_eq!(got, vec![("mixed@m".to_string(), true), ("remotion@remotion".to_string(), true)], "user installs whose folder is there; project, local and deleted ones are not");
    // The first version of the file holds one object per plugin, not an array.
    write_plugins(&root, &format!(r#"{{ "version": 1, "plugins": {{ "remotion@remotion": {{ "version": "1", "installPath": {install:?} }} }} }}"#));
    assert_eq!(installed_plugins(&root), vec![("remotion@remotion".to_string(), true)]);
    // A plugin the human switched off is installed, not ready.
    std::fs::write(root.join("settings.json"), r#"{ "enabledPlugins": { "remotion@remotion": false } }"#).unwrap();
    assert_eq!(installed_plugins(&root), vec![("remotion@remotion".to_string(), false)]);
    // Missing or broken files are nothing, never a panic.
    assert!(installed_plugins(&root.join("absent")).is_empty());
    write_plugins(&root, "{ nope");
    assert!(installed_plugins(&root).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remotion_is_found_in_any_roots_plugins_not_only_the_users() {
    let user = temp("user-root");
    let account = temp("account-root");
    let install = account.join("plugins").join("cache").join("remotion");
    std::fs::create_dir_all(&install).unwrap();
    write_plugins(&user, r#"{ "version": 2, "plugins": {} }"#);
    write_plugins(&account, &plugin_json(&install, &account.join("nowhere")));
    let hits = plugin_hits(&[user.clone(), account.clone()]);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].cover, cover("plugin:remotion@remotion", "remotion", READY));
    assert_eq!(hits[0].capabilities, vec!["compose"]);
    // The same plugin in two roots is one hit.
    let both = plugin_hits(&[account.clone(), account.clone()]);
    assert_eq!(both.len(), 1);
    // Without it in any root: nothing.
    assert!(plugin_hits(&[user.clone()]).is_empty());
    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(&account);
}

/// Managed Claude accounts set up, enabled and one active, under an id no machine has a root for.
fn managed_claude_on() -> (crate::state::Preferences, PathBuf) {
    let id = format!("test-{}", uuid::Uuid::new_v4().simple());
    let mut p = crate::state::Preferences { accounts_setup_complete: true, accounts_enabled: true, ..Default::default() };
    p.managed_accounts = vec![serde_json::from_value(serde_json::json!({ "id": id, "runtime": "claude", "label": "Work" })).unwrap()];
    p.active_account_ids.insert("claude".into(), id.clone());
    (p, crate::accounts::account_root(crate::accounts::Runtime::Claude, &id).unwrap())
}

/// A config root with Remotion installed at user scope.
fn root_with_remotion(tag: &str) -> PathBuf {
    let root = temp(tag);
    let install = root.join("plugins").join("cache").join("remotion");
    std::fs::create_dir_all(&install).unwrap();
    write_plugins(&root, &plugin_json(&install, &root.join("nowhere")));
    root
}

fn found(prefs: &crate::state::Preferences, users_own: &Path, home: &Path) -> bool {
    plugin_hits(&plugin_roots_for(prefs, Some(users_own), Some(home))).iter().any(|h| h.cover.provider == "remotion")
}

#[test]
fn a_plugin_is_found_only_in_the_root_the_coordinator_loads() {
    // `x` is the human's exported CLAUDE_CONFIG_DIR, `home` holds ~/.claude: Remotion in one each.
    let x = root_with_remotion("exported-x");
    let home = temp("home");
    let dot_claude = home.join(".claude");
    std::fs::create_dir_all(&dot_claude).unwrap();
    let empty_x = temp("empty-x");
    let plugin_home = temp("plugin-home");
    let install = plugin_home.join(".claude").join("plugins").join("cache").join("remotion");
    std::fs::create_dir_all(&install).unwrap();
    write_plugins(&plugin_home.join(".claude"), &plugin_json(&install, &plugin_home.join("nowhere")));

    // 1. Managed on, plus an exported /x: the coordinator loads the account's root (linked to
    //    ~/.claude), so a plugin only in /x is not found.
    let (on, account) = managed_claude_on();
    assert_eq!(plugin_roots_for(&on, Some(&x), Some(&home)), vec![account.clone()]);
    assert!(!found(&on, &x, &home), "a plugin only in the exported root is no coordinator's");

    // 2. Managed off, with /x: /x alone. Found there; a plugin only in ~/.claude is not.
    let off = crate::state::Preferences::default();
    assert_eq!(plugin_roots_for(&off, Some(&x), Some(&home)), vec![x.clone()]);
    assert!(found(&off, &x, &home));
    assert!(!found(&off, &empty_x, &plugin_home), "a plugin only in ~/.claude, with /x exported");

    // 3. Set up but switched off (or none active), with /x: as off, the account's root unread.
    let (mut disabled, _) = managed_claude_on();
    disabled.accounts_enabled = false;
    let (mut none_active, _) = managed_claude_on();
    none_active.active_account_ids.clear();
    for prefs in [&disabled, &none_active] {
        assert_eq!(plugin_roots_for(prefs, Some(&x), Some(&home)), vec![x.clone()]);
        assert!(found(prefs, &x, &home));
        assert!(!found(prefs, &empty_x, &plugin_home));
    }

    // Nothing exported, nothing managed: ~/.claude.
    assert_eq!(plugin_roots_for(&off, None, Some(&plugin_home)), vec![plugin_home.join(".claude")]);
    assert!(plugin_hits(&plugin_roots_for(&off, None, Some(&plugin_home))).iter().any(|h| h.cover.provider == "remotion"));

    for d in [x, home, empty_x, plugin_home] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn launch_status_and_install_name_the_same_root() {
    let x = Path::new("/x");
    let home = Path::new("/home/me");
    let (on, account) = managed_claude_on();
    let (mut disabled, _) = managed_claude_on();
    disabled.accounts_enabled = false;
    let off = crate::state::Preferences::default();
    for (prefs, users_own) in [(&on, Some(x)), (&on, None), (&off, Some(x)), (&off, None), (&disabled, Some(x)), (&disabled, None)] {
        let coordinator = crate::hive::media::install::coordinator_config_root(prefs, users_own, Some(home));
        // Launch: the same root, wherever launch names one.
        let launch = crate::hive::launch::config_root_for(prefs, users_own);
        assert_eq!(coordinator, launch.clone().or_else(|| Some(home.join(".claude"))), "{users_own:?}");
        // Status: that root alone.
        assert_eq!(plugin_roots_for(prefs, users_own, Some(home)), coordinator.clone().into_iter().collect::<Vec<_>>());
        // Install: that root, or ~/.claude (env -u) where it is a managed root linking there or
        // the default itself.
        let install = crate::hive::media::install::plugin_root(prefs, users_own);
        if coordinator.as_ref() == Some(&account) {
            assert_eq!(install, None, "a managed root is written through to ~/.claude");
        } else {
            assert_eq!(install.unwrap_or_else(|| home.join(".claude")), coordinator.clone().unwrap(), "{users_own:?}");
        }
    }
}

// ─── ComfyUI ────────────────────────────────────────────────────────────────

#[test]
fn comfyui_is_ready_when_it_answers_and_installed_when_only_the_app_is_there() {
    let apps = temp("apps");
    std::fs::create_dir_all(apps.join("ComfyUI.app")).unwrap();
    let running = tool("comfyui", "comfyui", "ComfyUI", Some("127.0.0.1:8188"), None);
    let up = comfy_hits(std::slice::from_ref(&running), std::slice::from_ref(&apps));
    assert_eq!(up.len(), 1, "one tool, not a port and an app");
    assert_eq!(up[0].cover, cover("comfyui", "comfyui", READY));
    assert_eq!(up[0].capabilities, vec!["local"]);
    let app_only = comfy_hits(&[], std::slice::from_ref(&apps));
    assert_eq!(app_only.len(), 1);
    assert_eq!(app_only[0].cover, cover("app:ComfyUI.app", "comfyui", INSTALLED));
    assert!(comfy_hits(&[], &[temp("empty-apps")]).is_empty());
    assert!(comfy_hits(&[], &[]).is_empty(), "no application folders (not a Mac): skipped quietly");
    let _ = std::fs::remove_dir_all(&apps);
}

// ─── Recommendation ─────────────────────────────────────────────────────────

fn rec(cap: &str, covered: &[Cover], pays: &[&str]) -> (Option<String>, Vec<String>) {
    recommend(cap, covered, &pays.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

#[test]
fn with_nothing_ready_the_catalogues_default_is_recommended() {
    for (cap, want) in [
        ("video", "fal"),
        ("image", "fal"),
        ("lipsync", "fal"),
        ("upscale", "fal"),
        ("3d", "fal"),
        ("music", "fal"),
        ("sfx", "fal"),
        ("voice", "elevenlabs"),
        ("finish", "homebrew-kit"),
        ("transcribe", "homebrew-kit"),
        ("compose", "remotion"),
    ] {
        assert_eq!(rec(cap, &[], &[]).0.as_deref(), Some(want), "{cap}");
    }
}

#[test]
fn a_ready_tool_means_nothing_to_recommend() {
    assert_eq!(rec("video", &[cover("mcp:fal-dev", "fal", READY)], &[]), (None, Vec::new()));
    // Even next to a subscriber one that isn't ready.
    assert_eq!(rec("video", &[cover("mcp:fal-dev", "fal", READY), cover("mcp:higgsfield", "higgsfield", NEEDS_SIGN_IN)], &["kling"]), (None, Vec::new()));
}

#[test]
fn a_tool_that_is_not_ready_still_leaves_the_capability_to_set_up() {
    for state in [NEEDS_SIGN_IN, UNREACHABLE, INSTALLED] {
        assert_eq!(rec("voice", &[cover("mcp:elevenlabs", "elevenlabs", state)], &[]).0.as_deref(), Some("elevenlabs"), "{state}");
    }
}

#[test]
fn a_subscriber_provider_is_promoted_and_fal_comes_first_among_the_alternatives() {
    // Listed as already paid for.
    let (r, alts) = rec("video", &[], &["higgsfield"]);
    assert_eq!(r.as_deref(), Some("higgsfield"));
    assert_eq!(alts.first().map(String::as_str), Some("fal"));
    assert!(!alts.contains(&"higgsfield".to_string()));
    // Detected, in any state short of ready.
    for state in [NEEDS_SIGN_IN, UNREACHABLE, INSTALLED] {
        let (r, alts) = rec("video", &[cover("mcp:runway", "runway", state)], &[]);
        assert_eq!(r.as_deref(), Some("runway"), "{state}");
        assert_eq!(alts.first().map(String::as_str), Some("fal"), "{state}");
    }
    // Two subscriptions: the first in catalogue order wins, the other follows fal.
    let (r, alts) = rec("video", &[], &["runway", "kling"]);
    assert_eq!((r.as_deref(), &alts[..2]), (Some("kling"), &["fal".to_string(), "runway".to_string()][..]));
    // Only for the capabilities it covers: Kling makes no images, so image stays fal.
    assert_eq!(rec("image", &[], &["kling"]).0.as_deref(), Some("fal"));
    // Higgsfield makes images too.
    assert_eq!(rec("image", &[], &["higgsfield"]).0.as_deref(), Some("higgsfield"));
    // A provider that is not a subscriber one is never promoted by being paid for.
    assert_eq!(rec("video", &[], &["wavespeed"]).0.as_deref(), Some("fal"));
    // Nor an id the catalogue doesn't know.
    assert_eq!(rec("video", &[], &["sora"]).0.as_deref(), Some("fal"));
}

#[test]
fn alternatives_are_the_other_recommendable_providers() {
    let (r, alts) = rec("video", &[], &[]);
    assert_eq!(r.as_deref(), Some("fal"));
    assert!(!alts.contains(&"fal".to_string()));
    assert_eq!(alts.first().map(String::as_str), Some("wavespeed"), "the others offered as listed, in catalogue order: {alts:?}");
    for id in ["wavespeed", "higgsfield", "kling", "runway"] {
        assert!(alts.contains(&id.to_string()), "{id}: {alts:?}");
    }
    assert!(!alts.contains(&"minimax".to_string()), "a manual route is a guide, not an alternative");
}

#[test]
fn a_manual_not_offered_or_unrecommendable_provider_is_never_recommended() {
    for cap in catalogue().capabilities.iter().map(|c| c.id.as_str()) {
        let (r, alts) = rec(cap, &[], &["minimax", "meshy", "google-genmedia", "kie", "replicate", "comfyui", "blender"]);
        for id in r.iter().chain(alts.iter()) {
            let p = provider(id).unwrap();
            assert!(p.offer != "detect_only" && p.recommendable && p.has_automatic_route(), "{cap}: {id} must not be offered");
        }
    }
    // Even when it is detected, and even when it is the only thing that covers the capability.
    assert_eq!(rec("music", &[cover("mcp:minimax", "minimax", INSTALLED)], &["minimax"]).0.as_deref(), Some("fal"));
    assert_eq!(rec("local", &[], &[]).0, None, "ComfyUI is detect-only: nothing to recommend, only Comfy Cloud to list");
    assert_eq!(rec("local", &[], &[]).1, vec!["comfy-cloud".to_string()]);
    assert!(!rec("video", &[], &[]).1.contains(&"replicate".to_string()), "listed, not recommended");
}

// ─── The rows ───────────────────────────────────────────────────────────────

#[test]
fn there_is_one_row_per_catalogue_capability_in_catalogue_order() {
    let all = rows(&[], &[]);
    let ids: Vec<&str> = all.iter().map(|r| r.capability.as_str()).collect();
    assert_eq!(ids, catalogue().capabilities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>());
    assert_eq!(ids.len(), 13);
    assert!(all.iter().all(|r| r.covered_by.is_empty()));
    assert_eq!(all.iter().find(|r| r.capability == "voice").unwrap().recommended.as_deref(), Some("elevenlabs"));
}

#[test]
fn the_row_shape_is_exactly_the_interface() {
    let hits = proxy_hits(&[server("fal", "https://mcp.fal.ai/mcp-relay", "needs_sign_in")], None);
    let all = rows(&hits, &["higgsfield".to_string()]);
    let video = serde_json::to_value(all.iter().find(|r| r.capability == "video").unwrap()).unwrap();
    let keys: Vec<&str> = video.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["alternatives", "capability", "covered_by", "recommended"]);
    assert_eq!(video["recommended"], "higgsfield");
    assert_eq!(video["alternatives"][0], "fal");
    let c = &video["covered_by"][0];
    assert_eq!(c.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), vec!["provider", "state", "tool_id"]);
    assert_eq!(c["state"], "needs_sign_in");
    // Nothing recommended is a null, not a missing key.
    let ready = rows(&proxy_hits(&[server("fal", "https://mcp.fal.ai/mcp-relay", "connected")], None), &[]);
    let v = serde_json::to_value(ready.iter().find(|r| r.capability == "video").unwrap()).unwrap();
    assert!(v["recommended"].is_null() && v["alternatives"].as_array().unwrap().is_empty());
}

#[test]
fn rows_join_every_source_and_drop_unknown_paid_ids() {
    let detected = [cli("ffmpeg"), cli("whisper-cli"), cli("blender"), tool("mcp:elevenlabs", "mcp", "elevenlabs", Some("api.elevenlabs.io"), None)];
    let mut hits = cli_hits(&detected);
    hits.extend(native_hits(&detected));
    hits.extend(comfy_hits(&[tool("comfyui", "comfyui", "ComfyUI", None, None)], &[]));
    let all = rows(&hits, &["not-a-provider".to_string()]);
    let row = |c: &str| all.iter().find(|r| r.capability == c).unwrap();
    assert_eq!(row("finish").covered_by, vec![cover("cli:ffmpeg", "homebrew-kit", READY)]);
    assert_eq!(row("finish").recommended, None);
    assert_eq!(row("transcribe").covered_by, vec![cover("cli:whisper-cli", "homebrew-kit", READY)]);
    assert_eq!(row("3d").covered_by, vec![cover("cli:blender", "blender", READY)]);
    assert_eq!(row("local").covered_by, vec![cover("comfyui", "comfyui", READY)]);
    assert_eq!(row("voice").covered_by, vec![cover("mcp:elevenlabs", "elevenlabs", INSTALLED)]);
    assert_eq!(row("voice").recommended.as_deref(), Some("elevenlabs"), "installed is not ready");
    assert_eq!(row("music").covered_by, vec![]);
}

/// What this Mac reports, for the report and for a look by eye: `cargo test --lib
/// hive::media::status::tests::this_mac -- --ignored --nocapture`. It reads the real machine and
/// asserts only the shape.
#[test]
#[ignore]
fn this_mac() {
    let prefs = crate::state::Preferences::default();
    let all = status(&prefs, None, &["higgsfield".to_string()]);
    assert_eq!(all.len(), catalogue().capabilities.len());
    println!("{}", serde_json::to_string_pretty(&all).unwrap());
}
