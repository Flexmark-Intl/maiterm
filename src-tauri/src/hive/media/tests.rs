use super::*;
use crate::hive::create::ProxyRef;

fn cat() -> &'static Catalogue {
    catalogue()
}

fn tool(id: &str, kind: &str, label: &str, host: Option<&str>, proxy: Option<ProxyRef>) -> DetectedTool {
    DetectedTool { id: id.into(), kind: kind.into(), label: label.into(), host: host.map(Into::into), pack: None, proxy }
}

fn proxied(name: &str, id: &str, upstream: &str) -> DetectedTool {
    tool(&format!("mcp:{name}"), "mcp", name, Some("127.0.0.1:8765"), Some(ProxyRef { id: id.into(), upstream: Some(upstream.into()) }))
}

fn proxy_facts(state: &str, workspaces: &[&str]) -> ProxyFacts {
    ProxyFacts { enabled: true, state: state.into(), workspaces: workspaces.iter().map(|w| w.to_string()).collect() }
}

fn row<'a>(s: &'a MediaStatus, capability: &str) -> &'a CapabilityRow {
    s.rows.iter().find(|r| r.capability == capability).unwrap()
}

fn facts_with(tools: Vec<DetectedTool>) -> Facts {
    Facts { tools, ..Default::default() }
}

#[test]
fn the_shipped_catalogue_parses_and_is_dated() {
    let c = parse(FILE).unwrap();
    assert_eq!(&c, cat(), "the runtime catalogue is the file");
    assert_eq!(c.capabilities.len(), 13, "the thirteen of §3");
    let common: Vec<&str> = c.capabilities.iter().filter(|c| c.common).map(|c| c.id.as_str()).collect();
    assert_eq!(common, vec!["video", "image", "voice", "music", "finish"], "the common five, first");
    for p in &c.providers {
        assert_eq!(p.checked, "2026-11-03", "{}: facts are dated", p.id);
        assert!(["recommend", "list", "detect_only"].contains(&p.offer.as_str()), "{}: offer {}", p.id, p.offer);
        for r in &p.routes {
            assert!(["remote_mcp", "brew", "plugin", "local_app", "manual"].contains(&r.kind.as_str()), "{}: route {}", p.id, r.kind);
            assert!(["oauth", "token", "none"].contains(&r.auth.as_str()), "{}: auth {}", p.id, r.auth);
            if r.kind == "remote_mcp" {
                assert!(r.url.starts_with("https://"), "{}: {}", p.id, r.url);
            }
        }
        if p.offer != "detect_only" {
            assert!(!p.routes.is_empty() && !p.guide.is_empty(), "{}: something to offer and a guide", p.id);
        }
        // A host nobody has confirmed is never also a sandbox host.
        assert!(p.unverified_hosts.iter().all(|h| !p.sandbox_hosts.contains(h)), "{}", p.id);
    }
}

#[test]
fn every_provider_in_the_spec_table_is_there() {
    for id in [
        "fal", "elevenlabs", "homebrew-kit", "remotion", "wavespeed", "higgsfield", "kling", "runway", "recraft", "ideogram", "bfl", "hedra", "heygen", "comfy-cloud", "replicate", "minimax", "meshy",
        "google-genmedia", "comfyui", "blender", "kie",
    ] {
        assert!(provider(id).is_some(), "{id}");
    }
    // The defaults of §4 and D3.
    for cap in ["video", "image", "lipsync", "upscale", "3d", "music", "sfx"] {
        assert!(provider("fal").unwrap().default_for.iter().any(|c| c == cap), "fal is the default for {cap}");
    }
    assert_eq!(provider("elevenlabs").unwrap().capabilities, vec!["voice"], "voice only for now (D3)");
    assert!(provider("remotion").unwrap().routes.iter().all(|r| r.kind == "plugin"));
    for id in ["higgsfield", "kling", "runway"] {
        assert!(provider(id).unwrap().subscriber_first, "{id} is shown first to a subscriber (D1)");
    }
    assert!(!provider("replicate").unwrap().recommendable);
}

#[test]
fn bad_files_are_refused() {
    assert!(parse("{}").is_err());
    assert!(parse(&FILE.replace("\"video\", \"image\", \"lipsync\"", "\"video\", \"hologram\", \"lipsync\"")).is_err(), "an unknown capability");
    assert!(parse(&FILE.replacen("\"id\": \"elevenlabs\"", "\"id\": \"fal\"", 1)).is_err(), "a provider twice");
    assert!(parse(&FILE.replacen("\"checked\": \"2026-11-03\",\n      \"offer\"", "\"checkd\": \"x\",\n      \"offer\"", 1)).is_err(), "a misspelt field");
}

/// HARD CONSTRAINT of phase 1: moving to the catalogue changes no sandbox host. These are the
/// hosts `KNOWN_GENERATOR_HOSTS` held before it was deleted, and the suffixes it matched by.
#[test]
fn todays_five_providers_keep_exactly_their_hosts() {
    let today: [(&str, &[&str], &[&str]); 5] = [
        ("fal", &["fal.ai", "fal.run", "fal.media"], &["fal.run", "queue.fal.run", "fal.media", "v3.fal.media"]),
        ("kie", &["kie.ai"], &["api.kie.ai"]),
        ("higgsfield", &["higgsfield.ai"], &["platform.higgsfield.ai"]),
        ("kling", &["klingai.com"], &["api.klingai.com"]),
        ("wavespeed", &["wavespeed.ai"], &["api.wavespeed.ai"]),
    ];
    for (id, suffixes, hosts) in today {
        let p = provider(id).unwrap();
        assert_eq!(p.sandbox_hosts, hosts, "{id}: sandbox_hosts are exactly today's");
        for s in suffixes {
            assert!(p.matches.hosts.iter().any(|h| h == s), "{id} keeps matching {s}");
        }
        assert_eq!(p.matches.names, vec![id], "{id} keeps matching its name word");
    }
    assert!(provider("kling").unwrap().matches.hosts.iter().any(|h| h == "kling.ai"), "kling's MCP host is matched too");
    // Nobody else has a sandbox host: the survey gave none a verified one, and unknown result
    // CDNs are never guessed in.
    let sandboxed: Vec<&str> = cat().providers.iter().filter(|p| !p.sandbox_hosts.is_empty()).map(|p| p.id.as_str()).collect();
    assert_eq!(sandboxed, vec!["fal", "wavespeed", "higgsfield", "kling", "kie"]);
    // The /generate skill still picks its models on Kie.ai.
    assert_eq!(providers_for_skill("generate").iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["kie"]);
}

#[test]
fn a_native_server_is_recognised_by_host_then_name() {
    let ids = |v: Vec<&Provider>| v.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(providers_for_native("anything", Some("mcp.fal.ai"))), vec!["fal"]);
    assert_eq!(ids(providers_for_native("x", Some("MCP.KLING.AI:443"))), vec!["kling"], "any case, any port, kling.ai");
    assert_eq!(ids(providers_for_native("x", Some("api.klingai.com"))), vec!["kling"]);
    assert_eq!(ids(providers_for_native("fal-mirror", Some("mcp.example.com"))), vec!["fal"], "by name word");
    assert_eq!(ids(providers_for_native("kling-video", None)), vec!["kling"], "a stdio entry: its name only");
    assert!(providers_for_native("github", Some("api.githubcopilot.com")).is_empty());
    assert!(providers_for_native("notfal", Some("fal.ai.evil.com")).is_empty(), "a suffix is a whole label");
}

#[test]
fn a_proxied_server_is_recognised_by_its_upstream_alone() {
    assert_eq!(providers_for_upstream(Some("https://mcp.fal.ai/mcp-relay"))[0].id, "fal");
    assert!(providers_for_upstream(Some("https://mcp.example.com/fal")).is_empty());
    assert!(providers_for_upstream(None).is_empty(), "a server the registry lost is nothing");
    assert!(providers_for_upstream(Some("not a url")).is_empty());
}

// ─── Status ─────────────────────────────────────────────────────────────────

#[test]
fn an_empty_machine_recommends_fal_for_what_fal_makes() {
    let s = build_status(cat(), &Facts::default(), None, &[]);
    assert_eq!(s.rows.len(), 13);
    assert_eq!(s.rows.iter().map(|r| r.capability.as_str()).collect::<Vec<_>>(), cat().capabilities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>());
    for cap in ["video", "image", "lipsync", "upscale", "3d", "music", "sfx"] {
        let r = row(&s, cap);
        assert_eq!(r.recommended.as_deref(), Some("fal"), "{cap}");
        assert!(r.covered_by.is_empty());
    }
    assert_eq!(row(&s, "voice").recommended.as_deref(), Some("elevenlabs"));
    assert_eq!(row(&s, "finish").recommended.as_deref(), Some("homebrew-kit"));
    assert_eq!(row(&s, "transcribe").recommended.as_deref(), Some("homebrew-kit"));
    assert_eq!(row(&s, "compose").recommended.as_deref(), Some("remotion"));
    assert_eq!(row(&s, "design").recommended.as_deref(), Some("recraft"));
    assert_eq!(row(&s, "design").alternatives, vec!["ideogram"]);
    // Nothing is the default for the local row: ComfyUI is detect-only, Comfy Cloud only listed.
    assert_eq!(row(&s, "local").recommended, None);
    assert_eq!(row(&s, "local").alternatives, vec!["comfy-cloud"]);
    // Alternatives: the others that make video, ones maiTerm can set up before a guide, and the
    // listed-but-not-recommendable one (Replicate) last. Subscribers are not promoted without evidence.
    let video = row(&s, "video");
    assert_eq!(video.alternatives, vec!["wavespeed", "higgsfield", "kling", "runway", "minimax", "google-genmedia", "replicate"]);
    assert!(!s.brew);
}

#[test]
fn a_detected_subscriber_is_promoted_over_fal_for_what_it_makes() {
    // Higgsfield is installed but not signed in: not usable yet, so video and image need a
    // recommendation, and it is Higgsfield's, not fal's (D1). Runway is absent: fal stays first.
    let mut f = facts_with(vec![proxied("higgs", "higgsfield", "https://mcp.higgsfield.ai/mcp")]);
    f.proxy.insert("higgsfield".into(), proxy_facts("needs_sign_in", &[]));
    let s = build_status(cat(), &f, None, &[]);
    for cap in ["video", "image"] {
        assert_eq!(row(&s, cap).recommended.as_deref(), Some("higgsfield"), "{cap}");
        assert!(row(&s, cap).alternatives.contains(&"fal".to_string()), "{cap}");
        assert_eq!(row(&s, cap).covered_by, vec![Cover { tool_id: "mcp:higgs".into(), provider: "higgsfield".into(), state: "needs_sign_in".into() }]);
    }
    assert_eq!(row(&s, "lipsync").recommended.as_deref(), Some("fal"), "it makes no lipsync");
    // A subscriber who is not installed yet, told to us: the later "I already pay for…" choice.
    let s = build_status(cat(), &Facts::default(), None, &["runway".to_string()]);
    assert_eq!(row(&s, "video").recommended.as_deref(), Some("runway"));
    assert_eq!(row(&s, "video").alternatives[0], "fal", "the default follows");
    assert_eq!(row(&s, "image").recommended.as_deref(), Some("fal"), "runway makes no images");
    // Any listed provider can be named, not only the three.
    let s = build_status(cat(), &Facts::default(), None, &["ideogram".to_string()]);
    assert_eq!(row(&s, "image").recommended.as_deref(), Some("ideogram"));
}

#[test]
fn something_usable_means_no_recommendation() {
    let mut f = facts_with(vec![proxied("video", "fal", "https://mcp.fal.ai/mcp-relay")]);
    f.proxy.insert("fal".into(), proxy_facts("connected", &[]));
    let s = build_status(cat(), &f, Some("w1"), &[]);
    for cap in ["video", "image", "lipsync", "upscale", "3d", "music", "sfx"] {
        let r = row(&s, cap);
        assert_eq!(r.covered_by.len(), 1, "{cap}");
        assert_eq!((r.covered_by[0].provider.as_str(), r.covered_by[0].state.as_str(), r.covered_by[0].tool_id.as_str()), ("fal", "ready", "mcp:video"));
        assert!(r.recommended.is_none() && r.alternatives.is_empty(), "{cap}: covered, nothing to recommend");
    }
    assert!(row(&s, "voice").recommended.is_some(), "what fal doesn't make is still missing");
}

#[test]
fn a_server_scoped_away_from_the_hive_is_unreachable_for_it_never_ready() {
    let mut f = facts_with(vec![proxied("fal", "fal", "https://mcp.fal.ai/mcp-relay")]);
    f.proxy.insert("fal".into(), proxy_facts("connected", &["other-workspace"]));
    let here = build_status(cat(), &f, Some("hive-ws"), &[]);
    assert_eq!(row(&here, "video").covered_by[0].state, "unreachable");
    assert_eq!(row(&here, "video").recommended.as_deref(), Some("fal"), "unusable here, so still missing: fix fal first");
    let there = build_status(cat(), &f, Some("other-workspace"), &[]);
    assert_eq!(row(&there, "video").covered_by[0].state, "ready");
    let prefs = build_status(cat(), &f, None, &[]);
    assert_eq!(row(&prefs, "video").covered_by[0].state, "ready", "Preferences applies no scope");
    // Every status maps as documented.
    let st = |state: &str, enabled: bool| proxy_state(Some(&ProxyFacts { enabled, state: state.into(), workspaces: vec![] }), None);
    assert_eq!(st("connected", true), "ready");
    assert_eq!(st("needs_sign_in", true), "needs_sign_in");
    assert_eq!(st("unreachable", true), "unreachable");
    assert_eq!(st("error", true), "unreachable");
    assert_eq!(st("connecting", true), "installed");
    assert_eq!(st("connected", false), "unreachable", "disabled");
    assert_eq!(proxy_state(None, None), "unreachable", "a server the registry lost");
}

#[test]
fn a_proxied_server_named_fal_that_points_elsewhere_covers_nothing() {
    let mut f = facts_with(vec![proxied("fal", "fal", "https://mcp.example.com/mcp"), proxied("video", "v", "https://mcp.fal.ai/mcp-relay")]);
    f.proxy.insert("fal".into(), proxy_facts("connected", &[]));
    f.proxy.insert("v".into(), proxy_facts("connected", &[]));
    let s = build_status(cat(), &f, None, &[]);
    assert_eq!(row(&s, "video").covered_by.iter().map(|c| c.tool_id.as_str()).collect::<Vec<_>>(), vec!["mcp:video"]);
}

#[test]
fn native_servers_clis_plugins_and_apps_cover_by_what_they_are() {
    let mut f = facts_with(vec![
        tool("mcp:fal", "mcp", "fal", Some("mcp.fal.ai"), None),
        tool("skill:generate", "skill", "generate", None, None),
        tool("mcp:github", "mcp", "github", Some("api.githubcopilot.com"), None),
    ]);
    f.clis = ["ffmpeg", "ffprobe", "whisper-cli", "blender"].iter().map(|s| s.to_string()).collect();
    f.plugins.insert("remotion@remotion".into());
    f.apps.insert("ComfyUI.app".into());
    f.brew = true;
    let s = build_status(cat(), &f, None, &[]);
    assert_eq!(row(&s, "video").covered_by.iter().map(|c| (c.tool_id.as_str(), c.state.as_str())).collect::<Vec<_>>(), vec![("mcp:fal", "installed"), ("skill:generate", "installed")]);
    assert_eq!(row(&s, "finish").covered_by, vec![
        Cover { tool_id: "cli:ffmpeg".into(), provider: "homebrew-kit".into(), state: "ready".into() },
        Cover { tool_id: "cli:ffprobe".into(), provider: "homebrew-kit".into(), state: "ready".into() },
    ]);
    assert_eq!(row(&s, "transcribe").covered_by[0].tool_id, "cli:whisper-cli");
    assert_eq!(row(&s, "3d").covered_by.iter().map(|c| c.tool_id.as_str()).collect::<Vec<_>>(), vec!["cli:blender", "mcp:fal"]);
    assert_eq!(row(&s, "compose").covered_by, vec![Cover { tool_id: "plugin:remotion@remotion".into(), provider: "remotion".into(), state: "installed".into() }]);
    assert_eq!(row(&s, "local").covered_by, vec![Cover { tool_id: "app:ComfyUI.app".into(), provider: "comfyui".into(), state: "installed".into() }]);
    assert!(row(&s, "compose").recommended.is_none());
    assert!(s.brew);
    // ComfyUI answering on its port is ready, and the app adds nothing beside it.
    f.comfy_running = true;
    let s = build_status(cat(), &f, None, &[]);
    assert_eq!(row(&s, "local").covered_by, vec![Cover { tool_id: "comfyui".into(), provider: "comfyui".into(), state: "ready".into() }]);
}

#[test]
fn a_ready_tool_sorts_before_a_broken_one_and_a_subscriber_wins_a_tie() {
    let mut f = facts_with(vec![
        proxied("a", "fal", "https://mcp.fal.ai/mcp-relay"),
        proxied("b", "wave", "https://api.wavespeed.ai/mcp"),
        proxied("c", "kl", "https://kling.ai/mcp"),
    ]);
    f.proxy.insert("fal".into(), proxy_facts("needs_sign_in", &[]));
    f.proxy.insert("wave".into(), proxy_facts("connected", &[]));
    f.proxy.insert("kl".into(), proxy_facts("connected", &[]));
    let s = build_status(cat(), &f, None, &[]);
    let order: Vec<&str> = row(&s, "video").covered_by.iter().map(|c| c.provider.as_str()).collect();
    assert_eq!(order, vec!["kling", "wavespeed", "fal"], "ready first; kling (a subscriber's) before wavespeed; fal needs a sign-in");
}

#[test]
fn plugins_are_read_from_a_roots_installed_plugins_file() {
    let root = std::env::temp_dir().join(format!("hive-media-root-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    std::fs::write(
        root.join("plugins/installed_plugins.json"),
        r#"{ "version": 2, "plugins": { "remotion@remotion": [{ "scope": "user" }], "swift-lsp@claude-plugins-official": [] } }"#,
    )
    .unwrap();
    let mut got = installed_plugins(&root);
    got.sort();
    assert_eq!(got, vec!["remotion@remotion", "swift-lsp@claude-plugins-official"]);
    assert!(installed_plugins(&root.join("nope")).is_empty());
    std::fs::write(root.join("plugins/installed_plugins.json"), "not json").unwrap();
    assert!(installed_plugins(&root).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
