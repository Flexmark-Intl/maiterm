use super::*;

fn cat() -> &'static Catalogue {
    catalogue()
}

#[test]
fn the_shipped_catalogue_parses_and_is_dated() {
    let c = parse(FILE).unwrap();
    assert_eq!(&c, cat(), "the runtime catalogue is the file");
    assert_eq!(c.capabilities.len(), 13, "the thirteen of §3");
    let common: Vec<&str> = c.capabilities.iter().filter(|c| c.common).map(|c| c.id.as_str()).collect();
    assert_eq!(common, vec!["video", "image", "voice", "music", "finish"], "the common five, first");
    for p in &c.providers {
        assert!(["2026-11-03", "2026-11-04"].contains(&p.checked.as_str()), "{}: facts are dated, got {}", p.id, p.checked);
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

/// The panel prints `name` then `means`: a meaning that only repeats the name reads "Music Music".
#[test]
fn every_capability_means_more_than_its_name() {
    for k in &cat().capabilities {
        let (name, means) = (k.name.to_ascii_lowercase(), k.means.to_ascii_lowercase());
        assert!(!means.trim().is_empty() && means != name && !means.starts_with(&format!("{name},")) && !means.starts_with(&format!("{name} ")), "{}: {:?} repeats {:?}", k.id, k.means, k.name);
    }
    assert_eq!(capability("music").unwrap().means, "Songs and background tracks");
    assert_eq!(capability("sfx").unwrap().means, "Short sounds like footsteps or clicks");
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
        assert_eq!(p.matches.own_hosts, suffixes, "{id}: a server's own host is a sandbox host under exactly the old suffixes");
    }
    // Nobody else's server host becomes a sandbox host, and kling.ai is recognised but is not one.
    let with_own: Vec<&str> = cat().providers.iter().filter(|p| !p.matches.own_hosts.is_empty()).map(|p| p.id.as_str()).collect();
    assert_eq!(with_own, vec!["fal", "wavespeed", "higgsfield", "kling", "kie"]);
    for h in ["api.elevenlabs.io", "mcp.runwayml.com", "mcp.kling.ai", "mcp.example.com"] {
        assert!(!is_sandbox_own_host(h), "{h}");
    }
    for h in ["mcp.fal.ai", "queue.fal.run", "api.klingai.com", "MCP.KIE.AI:443"] {
        assert!(is_sandbox_own_host(h), "{h}");
    }
    assert!(provider("kling").unwrap().matches.hosts.iter().any(|h| h == "kling.ai"), "kling's MCP host is matched too");
    // Nobody else has a sandbox host: the survey gave none a verified one, and unknown result
    // CDNs are never guessed in.
    let sandboxed: Vec<&str> = cat().providers.iter().filter(|p| !p.sandbox_hosts.is_empty()).map(|p| p.id.as_str()).collect();
    assert_eq!(sandboxed, vec!["fal", "wavespeed", "higgsfield", "kling", "kie"]);
    // The /generate skill still picks its models on Kie.ai.
    assert_eq!(providers_for_skill("generate").iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["kie"]);
}

/// Folded in on 2026-11-04 from a research pass over the providers' docs.
#[test]
fn fal_music_and_sfx_are_verified_and_name_their_models() {
    let fal = provider("fal").unwrap();
    for id in ["fal", "bfl", "runway", "meshy", "comfy-cloud", "wavespeed", "recraft"] {
        assert_eq!(provider(id).unwrap().checked, "2026-11-04", "{id}: its facts changed in the 2026-11-04 pass");
    }
    assert!(fal.capability_notes.is_empty(), "no UNVERIFIED note is left on fal: {:?}", fal.capability_notes);
    assert_eq!(fal.capability_models.get("music").map(String::as_str), Some("Lyria 3 and 3.5, ElevenLabs Music, MiniMax Music, Stable Audio"));
    assert_eq!(fal.capability_models.get("sfx").map(String::as_str), Some("ElevenLabs Sound Effects, Mirelo SFX, Stable Audio SFX"));
    for u in [
        "https://fal.ai/models/google/lyria-3.5",
        "https://fal.ai/models/fal-ai/lyria3",
        "https://fal.ai/models/elevenlabs/music/v2.5",
        "https://fal.ai/models/elevenlabs/sound-effects/v3",
    ] {
        assert!(fal.source_urls.iter().any(|s| s == u), "{u}");
    }
    assert!(fal.result_hosts.iter().any(|h| h == "v3b.fal.media"), "documented result host");
    // Result hosts are informational: the sandbox's set is still exactly phase 1's.
    assert_eq!(fal.sandbox_hosts, ["fal.run", "queue.fal.run", "fal.media", "v3.fal.media"]);
    // ElevenLabs stays voice only: its remote MCP documents speech and agents.
    assert_eq!(provider("elevenlabs").unwrap().capabilities, vec!["voice"]);
}

#[test]
fn result_hosts_nobody_has_confirmed_wait_in_unverified_hosts_and_never_reach_a_sandbox() {
    for (id, host, retention) in [
        ("bfl", "delivery.*.bfl.ai", ""),
        ("runway", "dnznrvs05pmza.cloudfront.net", ""),
        ("meshy", "assets.meshy.ai", "3 days"),
        ("comfy-cloud", "storage.googleapis.com", ""),
    ] {
        let p = provider(id).unwrap();
        assert_eq!(p.unverified_hosts, vec![host], "{id}");
        assert!(p.sandbox_hosts.is_empty(), "{id}: an unverified host is no sandbox host");
        assert!(p.result_hosts.is_empty(), "{id}: nor a confirmed result host");
        assert_eq!(p.retention, retention, "{id}");
    }
    // A pattern is not a host: no wildcard sits where a host is compared literally.
    for p in &cat().providers {
        for h in p.result_hosts.iter().chain(&p.sandbox_hosts).chain(&p.api_hosts) {
            assert!(!h.contains('*'), "{}: {h}", p.id);
        }
    }
}

#[test]
fn quote_and_balance_tools_are_named_on_the_route() {
    let route = |id: &str| provider(id).unwrap().routes[0].clone();
    assert_eq!(route("fal").quote_tool.as_deref(), Some("get_pricing"));
    assert_eq!(route("wavespeed").quote_tool.as_deref(), Some("get_price"));
    // get_credits and get_user read a balance: they price no job, so they are not quote tools.
    for (id, tool) in [("wavespeed", "get_balance"), ("bfl", "get_credits"), ("recraft", "get_user"), ("meshy", "meshy_check_balance")] {
        assert_eq!(route(id).balance_tool.as_deref(), Some(tool), "{id}");
    }
    for id in ["bfl", "recraft", "meshy"] {
        assert_eq!(route(id).quote_tool, None, "{id}");
    }
    assert!(provider("fal").unwrap().routes[0].balance_tool.is_none());
}

#[test]
fn oauth_stays_the_route_auth_where_a_token_also_works() {
    for id in ["wavespeed", "comfy-cloud"] {
        let p = provider(id).unwrap();
        assert_eq!(p.routes[0].auth, "oauth", "{id}");
        assert!(p.capability_notes.values().any(|n| n.contains("API key")), "{id}: the token alternative is noted");
    }
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

#[test]
fn lookups_by_id_capability_and_host() {
    assert_eq!(provider("fal").unwrap().name, "fal");
    assert!(provider("nope").is_none());
    assert_eq!(capability("music").unwrap().name, "Music");
    assert_eq!(common_capabilities().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["video", "image", "voice", "music", "finish"]);
    assert_eq!(providers_for_host("V3.FAL.MEDIA:443")[0].id, "fal");
    assert_eq!(providers_for_host("api.klingai.com")[0].id, "kling");
    assert!(providers_for_host("example.com").is_empty());
    assert!(provider("fal").unwrap().covers("music") && !provider("fal").unwrap().covers("voice"));
}
