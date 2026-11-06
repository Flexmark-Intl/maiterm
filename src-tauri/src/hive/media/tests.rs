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
        assert!(["2026-11-03", "2026-11-04", "2026-11-05"].contains(&p.checked.as_str()), "{}: facts are dated, got {}", p.id, p.checked);
        assert!(["recommend", "list", "detect_only"].contains(&p.offer.as_str()), "{}: offer {}", p.id, p.offer);
        for r in &p.routes {
            assert!(["remote_mcp", "stdio_mcp", "brew", "plugin", "local_app", "manual"].contains(&r.kind.as_str()), "{}: route {}", p.id, r.kind);
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
    for id in ["fal", "bfl", "runway", "comfy-cloud", "wavespeed", "recraft"] {
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

// ─── proposeHive's `media` (docs/hive-media.md §8) ──────────────────────────

fn ids(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_requested_media_list_keeps_known_ids_in_order() {
    let r = check_requested(&ids(&["music", "video"]));
    assert_eq!(r.capabilities, ids(&["music", "video"]));
    assert!(r.dropped.is_empty());
    assert_eq!(check_requested(&[]), RequestedMedia::default());
}

#[test]
fn a_requested_media_list_drops_unknown_ids_and_names_them() {
    let r = check_requested(&ids(&["video", "hologram", " music ", "", "Video", "hologram"]));
    assert_eq!(r.capabilities, ids(&["video", "music"]), "surrounding space is trimmed, ids are exact");
    assert_eq!(r.dropped, ids(&["hologram", "Video"]),"each unknown once, in the order met");
}

#[test]
fn a_requested_media_list_counts_a_repeat_once() {
    let r = check_requested(&ids(&["image", "image", "voice", "image"]));
    assert_eq!(r.capabilities, ids(&["image", "voice"]));
}

#[test]
fn a_requested_media_list_is_capped_at_the_number_of_capabilities() {
    let all: Vec<String> = cat().capabilities.iter().map(|c| c.id.clone()).collect();
    let mut asked = all.clone();
    asked.extend(all.iter().cycle().take(40).cloned());
    asked.extend((0..100).map(|i| format!("nope{i}-{}", "x".repeat(200))));
    let r = check_requested(&asked);
    assert_eq!(r.capabilities, all, "every capability once, never more than the catalogue has");
    assert_eq!(r.capabilities.len(), cat().capabilities.len());
    assert_eq!(r.dropped.len(), cat().capabilities.len(), "the dropped list is bounded too");
    assert!(r.dropped.iter().all(|d| d.chars().count() <= 40), "a long id is shortened in the note");
}

// ─── Phase 5: local MCP servers maiTerm runs itself (docs/hive-media.md §12.1) ─

fn route_ids(p: &Provider) -> Vec<String> {
    p.stdio_routes().filter_map(|r| r.id.clone()).collect()
}

#[test]
fn minimax_and_meshy_are_stdio_routes_maiterm_runs_with_a_key() {
    let mm = provider("minimax").unwrap();
    let meshy = provider("meshy").unwrap();
    assert_eq!(route_ids(mm), ids(&["minimax", "minimax-mainland"]));
    assert_eq!(route_ids(meshy), ids(&["meshy"]));
    for p in [mm, meshy] {
        assert!(p.routes.iter().all(|r| r.kind == STDIO_MCP), "{}: no manual route left", p.id);
        assert!(p.has_automatic_route(), "{}: offered as an alternative now", p.id);
        assert_eq!(p.checked, "2026-11-05");
        assert!(p.sandbox_hosts.is_empty() && p.unverified_hosts.is_empty(), "{}: its server runs outside every sandbox", p.id);
        for r in p.stdio_routes() {
            let spec = r.stdio_spec().unwrap();
            spec.validate().unwrap();
            assert_eq!(r.auth, "token", "{}: the token setup, then Verify", p.id);
            assert!(r.key_url.as_deref().is_some_and(|u| u.starts_with("https://")), "{}: its key page", p.id);
            assert!(r.results_note.is_some(), "{}: the brief says where its files land", p.id);
            // Pinned to an exact version: `pkg==1.2.3` or `pkg@1.2.3`.
            let pinned = spec.args.iter().any(|a| {
                let v = a.rsplit_once("==").or_else(|| a.rsplit_once('@').filter(|(pkg, _)| !pkg.is_empty())).map(|(_, v)| v).unwrap_or("");
                v.split('.').count() == 3 && v.split('.').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            });
            assert!(pinned, "{}: {:?} pins its package", p.id, spec.args);
        }
    }
    let spec = |id: &str| stdio_route(id).unwrap().1.stdio_spec().unwrap();
    assert_eq!((spec("minimax").command.as_str(), spec("minimax").args.clone()), ("uvx", ids(&["minimax-mcp==0.0.19"])));
    assert_eq!(spec("minimax").env_key.as_deref(), Some("MINIMAX_API_KEY"));
    assert_eq!(spec("meshy").args, ids(&["-y", "@meshy-ai/meshy-mcp-server@0.6.7"]));
    assert_eq!(spec("meshy").env_key.as_deref(), Some("MESHY_API_KEY"));
    // Meshy's host and transport are pinned, so a `.env` in its working folder (which dotenv
    // reads, never overriding what is set) can't send its key elsewhere.
    assert_eq!(spec("meshy").env.get("MESHY_API_HOST").map(String::as_str), Some("https://api.meshy.ai"));
    assert_eq!(spec("meshy").env.get("TRANSPORT").map(String::as_str), Some("stdio"));
    // Every path or URL argument the two packages take is ruled (docs/hive-media.md §12.1).
    let ruled = |id: &str| spec(id).arg_rules.iter().map(|r| format!("{}:{}:{}", r.tool, r.arg, r.rule)).collect::<Vec<_>>();
    assert_eq!(ruled("meshy"), ids(&[":save_to:save_inside_output", ":file_path:inside_output", ":file_paths:inside_output", ":reference_file_paths:inside_output"]));
    assert_eq!(
        ruled("minimax"),
        ids(&[
            ":output_directory:relative_dir",
            ":is_url:not_true",
            "voice_clone:file:inside_output",
            "generate_video:first_frame_image:url_or_inside_output",
            "play_audio::refuse_tool",
            "text_to_audio:text:name_part",
            "voice_clone:text:name_part",
            "voice_design:preview_text:name_part",
            "text_to_image:prompt:name_part",
            "query_video_generation:task_id:name_part",
            "text_to_audio:format:one_of",
        ])
    );
    assert_eq!(spec("minimax-mainland").arg_rules, spec("minimax").arg_rules, "both regions' servers, the same rules");
    // A `.env` the child's dotenv reads can't set what a request library or Node honours.
    for clear in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "NODE_TLS_REJECT_UNAUTHORIZED", "NODE_OPTIONS"] {
        assert_eq!(spec("meshy").env.get(clear).map(String::as_str), Some(""), "{clear} pinned empty");
    }
    // MiniMax writes files under its output folder (local mode), and a key is good on one host only.
    for (id, host) in [("minimax", "https://api.minimax.io"), ("minimax-mainland", "https://api.minimaxi.com")] {
        let env = spec(id).env;
        assert_eq!(env.get("MINIMAX_API_HOST").map(String::as_str), Some(host), "{id}");
        assert_eq!(env.get("MINIMAX_MCP_BASE_PATH").map(String::as_str), Some(crate::mcp_proxy::stdio::OUTPUT_PLACEHOLDER), "{id}");
        assert_eq!(env.get("MINIMAX_API_RESOURCE_MODE").map(String::as_str), Some("local"), "{id}");
    }
    // What its server really has: no music tool in minimax-mcp 0.0.19.
    assert_eq!(mm.capabilities, ids(&["video", "image", "voice"]));
    assert!(!mm.covers("music"));
    assert_eq!(meshy.result_hosts, ids(&["assets.meshy.ai"]));
    assert_eq!(meshy.retention, "3 days");
    // The checks Verify may call.
    assert_eq!(meshy.routes[0].balance_tool.as_deref(), Some("meshy_check_balance"));
    assert!(mm.stdio_routes().all(|r| r.check_tool.as_deref() == Some("list_voices") && r.balance_tool.is_none() && r.quote_tool.is_none()));
}

/// D3's outcome: ElevenLabs has no maintained stdio server for music or sfx (its own was archived,
/// its CLI has no stdio MCP mode), so it stays speech-only and fal keeps music and sfx. Google
/// genmedia needs a GCP project and a bucket, not a key: it stays a guide.
#[test]
fn elevenlabs_gets_no_stdio_route_and_genmedia_stays_manual() {
    let el = provider("elevenlabs").unwrap();
    assert_eq!(el.stdio_routes().count(), 0);
    assert_eq!(el.capabilities, ids(&["voice"]));
    assert!(provider("fal").unwrap().default_for.iter().any(|c| c == "music"));
    assert!(provider("google-genmedia").unwrap().routes.iter().all(|r| r.kind == "manual"));
}

#[test]
fn a_stdio_server_is_recognised_by_its_route_and_nothing_else() {
    let ids_of = |v: Vec<&Provider>| v.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids_of(providers_for_proxied(Some("stdio:meshy"))), vec!["meshy"]);
    assert_eq!(ids_of(providers_for_proxied(Some("stdio:minimax-mainland"))), vec!["minimax"]);
    assert!(providers_for_proxied(Some("stdio:fal")).is_empty(), "a route id, not a provider's name");
    assert!(providers_for_proxied(Some("stdio:")).is_empty());
    assert!(providers_for_proxied(Some("stdio:nope")).is_empty());
    assert!(providers_for_proxied(None).is_empty());
    // An http upstream is still read by its host.
    assert_eq!(ids_of(providers_for_proxied(Some("https://mcp.fal.ai/mcp-relay"))), vec!["fal"]);
    // A host lookup never takes a route for a URL.
    assert!(providers_for_upstream(Some("stdio:meshy")).is_empty());
    assert!(stdio_route("meshy").is_some_and(|(p, r)| p.id == "meshy" && r.kind == STDIO_MCP));
    assert!(stdio_route("fal").is_none(), "a remote route has no stdio id");
}

/// A route the proxy would refuse fails here, in a test, never at a human's setup.
#[test]
fn a_bad_stdio_route_fails_the_catalogue_parse() {
    let with = |route: serde_json::Value| {
        let mut c: serde_json::Value = serde_json::from_str(FILE).unwrap();
        let meshy = c["providers"].as_array_mut().unwrap().iter_mut().find(|p| p["id"] == "meshy").unwrap();
        meshy["routes"] = serde_json::json!([route]);
        parse(&c.to_string())
    };
    let good = serde_json::json!({ "kind": "stdio_mcp", "id": "meshy", "command": "npx", "args": ["-y", "x@1.0.0"], "auth": "token", "credits": "api", "env_key": "MESHY_API_KEY" });
    assert!(with(good.clone()).is_ok());
    let bad = |patch: serde_json::Value| {
        let mut r = good.clone();
        for (k, v) in patch.as_object().unwrap() {
            if v.is_null() {
                r.as_object_mut().unwrap().remove(k);
            } else {
                r[k] = v.clone();
            }
        }
        with(r).unwrap_err()
    };
    assert!(bad(serde_json::json!({ "id": null })).contains("needs an id"));
    assert!(bad(serde_json::json!({ "command": null })).contains("needs an id and a command"));
    assert!(bad(serde_json::json!({ "env_key": "PATH" })).contains("route meshy"), "a key in PATH");
    assert!(bad(serde_json::json!({ "env_key": "MESHY_API" })).contains("_KEY"), "not a key's name");
    assert!(bad(serde_json::json!({ "env": { "NODE_OPTIONS": "--require /tmp/x.js" } })).contains("NODE_OPTIONS"));
    assert!(bad(serde_json::json!({ "args": ["a\nb"] })).contains("line break"));
    assert!(bad(serde_json::json!({ "env_key": null })).contains("names no variable"), "a key route with nowhere for the key");
    assert!(bad(serde_json::json!({ "key_url": "http://x.example/keys" })).contains("https"));
    assert!(bad(serde_json::json!({ "id": "minimax" })).contains("appears twice"), "route ids are unique in the catalogue");
}
