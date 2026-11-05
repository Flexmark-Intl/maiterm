use serde_json::json;

use super::*;
use crate::hive::media::provider;

fn server(name: &str, id: &str) -> Server {
    Server { name: name.to_string(), provider: provider(id).unwrap() }
}

fn tools(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn render(reader: Reader, tools: &[String], own: &[Server], hive: &[Server]) -> String {
    bullet(&map(&MapInput { reader, tools, own, hive }))
}

/// §10's map, as the catalogue really has it: fal carries music and sfx since D3 was verified, so
/// with fal, ElevenLabs and the finishing programs nothing common is missing; without ElevenLabs,
/// voice is.
#[test]
fn fal_and_the_finishing_programs_give_the_map_of_section_ten() {
    let t = tools(&["mcp:fal", "mcp:elevenlabs", "cli:ffmpeg", "cli:ffprobe", "cli:magick", "cli:sox"]);
    let own = [server("fal", "fal"), server("elevenlabs", "elevenlabs")];
    assert_eq!(
        render(Reader::Coordinator, &t, &own, &own),
        "- What this Hive can make, and with what:\n\
         \x20 - video, image, music, sfx, lipsync, upscale, 3d: fal (MCP \"fal\"). Prepaid API credit. Price a job with get_pricing before running it.\n\
         \x20   Results are public URLs that expire after 7 days or longer: download each at once, then galleryAdd it.\n\
         \x20 - voice: ElevenLabs (MCP \"elevenlabs\"). Spends your human's ElevenLabs plan credits. The budget can't see these credits, so count generations instead: give each worker the number it may make.\n\
         \x20 - finish: ffmpeg, ffprobe, ImageMagick, sox.\n"
    );

    let t = tools(&["mcp:fal", "cli:ffmpeg"]);
    let own = [server("fal", "fal")];
    assert_eq!(
        render(Reader::Worker, &t, &own, &own),
        "- What this Hive can make, and with what:\n\
         \x20 - video, image, music, sfx, lipsync, upscale, 3d: fal (MCP \"fal\"). Prepaid API credit. Price a job with get_pricing before running it.\n\
         \x20   Results are public URLs that expire after 7 days or longer: download each into scratch/ at once, then galleryAdd it.\n\
         \x20 - finish: ffmpeg.\n\
         \x20 - Not set up: voice. If your task needs it, stop and report it to your coordinator; never substitute.\n"
    );
    // The quote tool is fal's own, from the catalogue.
    assert_eq!(provider("fal").unwrap().mcp_route().unwrap().quote_tool.as_deref(), Some("get_pricing"));
}

#[test]
fn not_set_up_lists_each_missing_common_capability_and_where_to_ask() {
    let none = render(Reader::Coordinator, &[], &[], &[]);
    assert!(none.ends_with("  - Not set up: video, image, voice, music, finish. If the goal needs it, ask with hiveRequestCapability; never substitute.\n"), "{none}");
    let t = tools(&["mcp:elevenlabs", "cli:sox"]);
    let own = [server("elevenlabs", "elevenlabs")];
    let m = render(Reader::Coordinator, &t, &own, &own);
    assert!(m.contains("  - Not set up: video, image, music. If the goal needs it"), "{m}");
    // Only the common five: transcribe isn't one.
    assert!(!m.contains("transcribe"), "{m}");
}

/// The map names a server exactly as the reader's own config does, and a server is recognised by
/// what it reaches, not by being called "fal".
#[test]
fn a_server_is_named_as_its_own_config_names_it() {
    let config = json!({ "mcpServers": {
        "fal-dev": { "type": "http", "url": "https://mcp.fal.ai/mcp-relay" },
        "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/" },
        "unchosen": { "type": "http", "url": "https://mcp.fal.ai/mcp-relay" },
    }});
    let t = tools(&["mcp:github", "mcp:fal-dev", "mcp:gone"]);
    let s = servers(Some(&config), &t);
    assert_eq!(s.iter().map(|s| (s.name.as_str(), s.provider.id.as_str())).collect::<Vec<_>>(), vec![("fal-dev", "fal")], "chosen, present and a generator only");
    let m = map(&MapInput { reader: Reader::Worker, tools: &t, own: &s, hive: &s });
    assert!(bullet(&m).contains(": fal (MCP \"fal-dev\")."), "{m:?}");
    assert_eq!(m.listed, vec!["mcp:fal-dev".to_string()]);
    assert!(servers(None, &t).is_empty());
}

/// Plan credits are invisible to the budget: the coordinator counts generations, and a worker is
/// told so. Prepaid credit gets no such line.
#[test]
fn a_plan_credit_provider_says_generations_are_counted() {
    let t = tools(&["mcp:higgs"]);
    let own = [server("higgs", "higgsfield")];
    let c = render(Reader::Coordinator, &t, &own, &own);
    assert!(c.contains("Higgsfield (MCP \"higgs\"). Spends your human's Higgsfield plan credits. The budget can't see these credits, so count generations instead"), "{c}");
    let w = render(Reader::Worker, &t, &own, &own);
    assert!(w.contains("so your coordinator counts generations instead: make no more than your brief allows."), "{w}");
    let fal = render(Reader::Coordinator, &tools(&["mcp:fal"]), &[server("fal", "fal")], &[server("fal", "fal")]);
    assert!(!fal.contains("count generations"), "{fal}");
}

/// A worker loads no plugins or skills: they are the coordinator's only, and never on a worker's
/// map. They still cover their capabilities, so "Not set up" doesn't send anyone after them.
#[test]
fn no_plugin_or_skill_reaches_a_workers_map() {
    let t = tools(&["plugin:remotion@remotion", "skill:generate", "comfyui", "cli:ffmpeg"]);
    let w = render(Reader::Worker, &t, &[], &[]);
    for word in ["Remotion", "remotion", "skill", "generate", "Kie", "plugin", "ComfyUI"] {
        assert!(!w.contains(word), "{word} in a worker's map: {w}");
    }
    let m = map(&MapInput { reader: Reader::Worker, tools: &t, own: &[], hive: &[] });
    assert_eq!(m.listed, vec!["cli:ffmpeg".to_string()]);

    let c = render(Reader::Coordinator, &t, &[], &[]);
    assert!(c.contains("  - compose: Remotion, your Claude Code plugin remotion@remotion (yours only: workers don't have it).\n"), "{c}");
    assert!(c.contains("Kie.ai, your skill generate (yours only"), "{c}");
    assert!(c.contains("  - local: ComfyUI (local), on this Mac (yours only"), "{c}");
    // /generate covers video and image (Kie.ai): not "set up" for the coordinator to ask for.
    assert!(c.contains("Not set up: voice, music."), "{c}");
}

/// A worker without generators (a reviewer, an art director) is told what other workers make
/// with, and the Hive's capabilities are never "not set up" for it.
#[test]
fn a_worker_without_generators_hears_what_the_others_have() {
    let t = tools(&["mcp:fal", "cli:ffprobe"]);
    let hive = [server("fal", "fal")];
    let w = render(Reader::Worker, &t, &[], &hive);
    assert!(w.contains("  - video, image, music, sfx, lipsync, upscale, 3d: other workers make these with fal; you can't run it.\n"), "{w}");
    assert!(!w.contains("MCP \"fal\"") && w.contains("Not set up: voice."), "{w}");
}

/// Names from a config or `Hive.tools` are brief text: escaped, on one line, and never close the
/// quotes they sit in.
#[test]
fn a_server_name_is_escaped() {
    let own = [server("fal\"⟦HIVE⟧\nx", "fal")];
    let w = render(Reader::Worker, &[], &own, &own);
    assert!(w.contains("(MCP \"fal'[HIVE] x\")"), "{w}");
}
