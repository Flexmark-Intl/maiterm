# Hive media setup: what it can make, and getting the rest

Design, 2026-11-03 (Darryl's direction). Nothing here is built yet. It extends `docs/hive.md`
§3 (the Tools step), §10.3 (how agents make assets) and §14.16 (the sandbox's generator hosts).

## 1. What it is

A Hive that makes images, video or audio is only as good as the tools its agents can really run.
Today the wizard's Tools step lists whatever it finds:
- MCP servers;
- skills;
- four CLIs (ffmpeg, magick, sox, blender);
- ComfyUI.

It says nothing about what is missing, and it gives no route to get it.

This feature turns that list into a **media setup** organised by what the Hive can make:
1. **Detect.** For each capability (video, image, voice, music…), say what on this machine
   already covers it, and whether that tool is ready or needs a sign-in.
2. **Offer.** The detected tools are ticked for the Hive, as today, but shown by capability.
3. **Recommend.** A missing capability gets a recommended provider, with one-click alternatives.
4. **Set up.** Setup is as automated as the provider allows: a browser sign-in through maiTerm's
   MCP proxy, a pasted key into the keychain, or a Homebrew install in a visible tab. Each route
   has a short guide for the steps only the human can do, such as making an account or adding
   credit.
5. **Verify.** A free check confirms the tool works before the Hive relies on it.

The coordinator's brief then gets a **capability map** in place of the raw tool ids. It says
which tool makes video, which models it carries, what it costs, and how long results last.

## 2. Principles

1. **The human sets up, maiTerm automates the clicks.** No agent signs in, pastes a key or
   installs anything. Hive agents can *ask* for a missing capability (§8), and the human's click
   starts the setup. This is the same rule as `addMcpServer` and docs/agent-requests.md.
2. **No API key in a plaintext config.** Any key goes into the keychain through the MCP proxy's
   token storage (docs/mcp-proxy.md §5). The recommended routes are all remote MCP servers with
   OAuth, so most setups involve no key at all. A provider reachable only by a local MCP server
   with an env key is listed as "manual" until the proxy grows stdio upstreams (§6, phase 5).
3. **Say which money it spends.** Several OAuth MCP servers spend the human's **subscription
   credits**, not an API balance; others use prepaid API credit. Each route says which, in
   maiTerm's words, before setup and on the capability row.
4. **One catalogue.** Providers, their routes, hosts, capabilities and guides live in one data
   file. It replaces `KNOWN_GENERATOR_HOSTS` and `SKILL_GENERATORS` (`hive/create.rs`), so
   detection, the sandbox's hosts, the worker's MCP copy and the brief all read one table.
5. **Facts in the catalogue are dated.** Providers change monthly: OpenAI shut down Sora's API on
   2026-09-24, and Tripo v2 closed on 2026-11-01. Each entry carries `checked: <date>`, and its
   unverified hosts are marked as such until a real job confirms them (§9).

## 3. Capabilities

| id | Means | Typical tools |
|---|---|---|
| `video` | Text or image to video | fal (Kling 4.0, Veo 3.1, Seedance 2.5), WaveSpeed, Higgsfield, Kling, Runway |
| `image` | Text to image, image editing | fal (GPT Image 2, FLUX, Nano Banana Pro), BFL, Ideogram, Higgsfield |
| `design` | Vectors, logos, typography | Recraft, Ideogram |
| `voice` | Text to speech, voice design | ElevenLabs, MiniMax |
| `music` | Music | ElevenLabs Music, MiniMax, Lyria via fal (Suno and Udio have no usable API: never offered) |
| `sfx` | Sound effects | ElevenLabs |
| `lipsync` | Lipsync, talking avatars | fal (sync.so), Hedra, HeyGen |
| `upscale` | Image or video upscaling | fal, Topaz |
| `3d` | Models | fal, Meshy, Blender |
| `transcribe` | Speech to text, captions | whisper.cpp |
| `finish` | Cut, convert, mix, check | ffmpeg/ffprobe, ImageMagick, sox |
| `compose` | Titles, motion graphics, programmatic edits | Remotion |
| `local` | Generation on this Mac | ComfyUI (local or Comfy Cloud MCP) |

The first page shows the common five (video, image, voice, music, finish). The rest are under
"More".

## 4. The catalogue

`src-tauri/resources/hive/media/catalogue.json` is loaded by Rust (`hive/media.rs`), and its
parse is tested as the role files are. One entry:

```jsonc
{
  "id": "fal", "name": "fal", "checked": "2026-11-03",
  "capabilities": ["video", "image", "lipsync", "upscale", "3d", "music"],
  "models_note": "Kling 4.0, Veo 3.1, Seedance 2.5, GPT Image 2, FLUX, sync.so",
  "routes": [{
    "kind": "remote_mcp",                // remote_mcp | brew | plugin | local_app | manual
    "url": "https://mcp.fal.ai/mcp-relay",
    "auth": "oauth",                     // oauth | token | none
    "credits": "api",                    // api (prepaid balance) | plan (subscription credits)
    "quote_tool": "get_pricing"          // a tool that prices a job before it runs, if any
  }],
  "result_hosts": ["fal.media", "v3.fal.media", "v3b.fal.media"],
  "api_hosts": ["fal.run", "queue.fal.run", "api.fal.ai"],
  "unverified_hosts": [],
  "retention": "7 days or longer",       // how long a result URL lives: the brief's "save at once"
  "signup_url": "https://fal.ai/login", "billing_url": "https://fal.ai/dashboard/billing",
  "guide": ["Make a fal account (or sign in).", "Add credit: fal bills a prepaid balance.", "Back here, press Sign in."],
  "match": { "hosts": ["fal.ai", "fal.run", "fal.media"], "names": ["fal"] }
}
```

The starting entries, from the 2026-11-03 survey:

| Provider | Route | Auth | Credits | Default for |
|---|---|---|---|---|
| **fal** | remote MCP `mcp.fal.ai/mcp-relay` | OAuth | API (prepaid), pricing tool | **video, image, lipsync, upscale, 3d** |
| **ElevenLabs** | remote MCP `api.elevenlabs.io/v1/mcp` | OAuth | plan | **voice** (music and sfx: see D3) |
| **Homebrew kit** | `brew install ffmpeg imagemagick sox whisper-cpp` | none | free | **finish, transcribe** |
| **Remotion** | Claude Code plugin `remotion@remotion` | none | free | **compose** |
| WaveSpeed | remote MCP `api.wavespeed.ai/mcp` | OAuth or token | API, `get_price` before running | alternative to fal |
| Higgsfield | remote MCP `mcp.higgsfield.ai/mcp` | OAuth | **plan** | video and image for an existing subscriber |
| Kling | remote MCP `kling.ai/mcp` | OAuth | **plan** | video for an existing subscriber |
| Runway | remote MCP `mcp.runwayml.com/mcp` | OAuth | **plan** (Pro/Max) | video for an existing subscriber |
| Recraft | remote MCP `mcp.recraft.ai/mcp` | OAuth | **plan** | design |
| Ideogram | remote MCP `mcp.ideogram.ai/mcp` | OAuth | **plan** | design, image |
| BFL (FLUX) | remote MCP `mcp.bfl.ai` (path unverified) | OAuth | API | image |
| Hedra | remote MCP `mcp.hedra.com` | OAuth | plan or wallet | lipsync |
| HeyGen | remote MCP `mcp.heygen.com/mcp/v1/` | OAuth | **plan** | lipsync, avatars |
| Comfy Cloud | remote MCP `cloud.comfy.org/mcp` | OAuth or token | Comfy credits | local / workflows |
| Replicate | remote MCP `mcp.replicate.com/sse` (SSE) | key pasted on their page | API | listed, not recommended: results expire after 1 h, and the proxy speaks Streamable HTTP only |
| MiniMax, Meshy, Google genmedia | local MCP with an env key | key | API | **manual** until stdio proxying (phase 5) |
| ComfyUI (local), Blender | detected only; guide links | none | free | local, 3d |
| Kie.ai, Luma, Topaz, Stability | not offered (no official MCP, or no cost API) | | | detected if present, as today |

Sources and the full survey: the research notes of 2026-11-03, quoted in each entry's
`source_urls`.

**Why fal as the default rather than Higgsfield.**
- fal signs in with OAuth and bills a prepaid API balance with a pricing API.
- One fal account carries the frontier video models (Kling 4.0, Veo 3.1, Seedance 2.5) within days
  of their release.
- Higgsfield resells many of the same models on top of the maker's price, and its MCP spends plan
  credits.

So Higgsfield, Kling direct and Runway are **promoted to the top for someone who already
subscribes** (detected, or picked by the human with "I already pay for…"). They aren't the
recommendation for someone starting from nothing. This is decision D1.

## 5. Detection

`hive_media_status` (Rust, off the main thread) answers one row per capability:

```
{ capability, covered_by: [{ tool_id, provider, state: ready|needs_sign_in|unreachable|installed }],
  recommended: provider_id?, alternatives: [provider_id] }
```

What it reads:
- **The existing detection** (`detect_tools`), with CLIs added: `whisper-cli`, `ffprobe` (part of
  ffmpeg) and `brew` itself, which the install route needs.
- **MCP proxy servers** (`mcp_proxy` registry): status and **upstream URL**. A proxied entry in
  `.claude.json` points at `127.0.0.1/mcp/x/<id>`, so it is matched to a catalogue provider by
  its upstream URL, never by the local one. Today `is_generator_server` vouches for a proxied
  entry by its *name* only. It switches to the upstream URL, so a server named "fal" that points
  elsewhere isn't trusted, and a fal server named "video" is.
- **Native `.claude.json` entries**, matched by `match.hosts`, then `match.names` (as today).
- **Installed Claude Code plugins** (`~/.claude/plugins`), for Remotion.
- **ComfyUI** on 127.0.0.1:8188 (as today), and the Comfy Desktop app's presence.

## 6. Setup routes

Each route is one button on a missing capability's recommendation card. The button opens a small
**setup sheet**: the guide's steps, the credits line, and the action.

| Route | What maiTerm does | What the human does |
|---|---|---|
| `remote_mcp` + `oauth` | Adds the server to the MCP proxy (docs/mcp-proxy.md §7, the human's own add path, so no request card) and opens the sign-in in the browser | Signs in; adds credit if the guide says so |
| `remote_mcp` + `token` | Opens the provider's key page; a paste field saves the key to the keychain as the proxy's bearer token | Makes the key, pastes it |
| `brew` | Opens a **visible maiTerm tab** running the one install command, then re-detects when it exits. With no Homebrew, the first step is Homebrew's own installer in the same tab, since it asks for the human's password | Watches; types a password if asked |
| `plugin` | Runs `claude plugin marketplace add …` and `claude plugin install …` in a visible tab | Watches |
| `local_app` | Shows the download link and guide; re-detects when the window regains focus | Installs the app |
| `manual` | Shows the guide, including the config snippet with the key left as a placeholder | Everything |

**Verify.** After a route finishes, maiTerm re-detects and, for an MCP server, lists its tools
through the proxy (free). Where the provider has a balance or pricing tool, it calls that too:
fal `get_pricing`, WaveSpeed `get_balance`. The row turns **Ready**, or shows the error in plain
words ("Signed in, but fal answered 402: add credit at fal.ai/dashboard/billing", with the link
as a button). It never runs a paid generation.

**Phase 5: stdio upstreams in the MCP proxy** (docs/mcp-proxy.md §2 lists them as v2). maiTerm
runs a local MCP server itself, with the key from the keychain in its environment. That moves
MiniMax, Meshy and Google genmedia from `manual` to `token`, and keeps those keys out of
`.claude.json`. It's the larger piece, so it comes last.

## 7. Where it appears

- **The Create sheet's Tools step**: a **What it can make** panel above the existing picker, which
  stays for everything else (code tools, skill packs). Detected media tools are ticked as today.
  For a Hive whose goal names no media and whose folders are code, the panel starts folded to one
  line ("Media: video, image ready · voice, music not set up"), so a code Hive isn't slowed.
  Whether to show it opened is an obvious-default rule in code (goal words, plain folders), not an
  AI judgment.
- **Hive settings → Tools** for an existing Hive. A tool set up here is added to `Hive.tools`, and
  its hosts to the gallery hosts. Workers pick it up at their next launch, as for any tools change
  (§3.4).
- **Preferences → Hive**, beside the default tools: the same panel, to set up a studio once for
  every future Hive.
- **The phone**: the status is visible, and a setup can be started that needs no browser sign-in.
  An OAuth sign-in waits for the Mac (docs/mcp-proxy.md §7's stated exception).

## 8. Agents asking for a capability

A worker that needs a tool the Hive lacks already stops and reports it (the media roles say so).
That report becomes a structured ask:
- **`hiveRequestCapability {capability, why}`**, for the coordinator only. It files an agent
  request of a new kind, `tool_setup`, whose card is maiTerm's words: "The Hive *Launch video*
  needs **music**. Recommended: ElevenLabs (signs in with your ElevenLabs plan's credits). Set up…".
  The agent's `why` is quoted.
- **Approving opens the setup sheet** on the desktop. Approving from the phone starts it there if
  no browser step is needed; otherwise it waits for the Mac.
- When the capability becomes Ready, it is added to the Hive, and the coordinator gets a framed
  notice ("music is now available through elevenlabs; your workers launched from now on have it").
- `proposeHive` (§3.7) gains an optional `media: [capability]`, so a planning agent's proposal opens
  the sheet with those capabilities' panel unfolded.

## 9. Sandbox hosts

- A **proxied** server's API calls leave from maiTerm, not the worker. So the worker's
  `allowedDomains` need only the catalogue's `result_hosts`, to download what the server returns.
  A native or local server needs `api_hosts` too.
- **Unverified hosts** (WaveSpeed, Higgsfield, Runway and Kie result CDNs, among others) aren't
  guessed. The existing sandbox-block flow catches the first refusal: a blocker goes to the queen,
  who grants the host. The catalogue is then corrected from that real URL in a follow-up commit.
  Phase 1 includes a spike: one cheapest real job per default provider, logging the result URL's
  host.
- whisper.cpp model downloads need `huggingface.co`. maiTerm fetches the model at install time,
  outside the sandbox, so no worker needs that host.

## 10. The brief

The coordinator's and every media worker's brief replace "Tools this Hive has: mcp:fal,
cli:ffmpeg" with a map:

```
What this Hive can make, and with what:
- video, image, lipsync, upscale: fal (MCP "fal"). Prepaid API credit. Price a job with get_pricing before running it.
  Results are public URLs that expire: download each into scratch/ at once, then galleryAdd it.
- voice: elevenlabs (MCP "elevenlabs"). Spends your human's ElevenLabs plan credits.
- finish: ffmpeg, ffprobe, ImageMagick, sox.
- Not set up: music. If the goal needs it, ask with hiveRequestCapability; never substitute.
```

The budget rule (§10.3) is unchanged. The map adds **quote before spending** wherever the
provider has a quote tool. For plan-credit providers it says the budget can't see those credits,
so the coordinator counts generations instead.

## 11. Build order

1. **Catalogue + detection.** The catalogue file, `hive/media.rs`, `hive_media_status`,
   upstream-URL matching for proxied servers, and the host and worker-MCP code moved onto the
   catalogue (no change in behaviour for today's five providers). Plus the result-host spike.
2. **The panel.** "What it can make" in the Tools step, Hive settings and Preferences, read-only:
   detection, recommendations, credits lines and guides.
3. **Setup routes.** `remote_mcp` (OAuth and token) through the proxy, `brew` and `plugin` in a
   visible tab, verification, and adding to the Hive.
4. **Agents.** The capability map in the briefs, `hiveRequestCapability` + `tool_setup` requests
   (Loom and phone, a protocol bump), and `proposeHive`'s `media`.
5. **Stdio upstreams in the MCP proxy**, so `manual` providers become `token`.

Every phase gets a subagent review. The sheets are Svelte, and every sheet change so far had
ordering bugs that only review found.

## 12. Decisions for Darryl

- **D1. Default video and image route:** fal (recommended, §4), with Higgsfield, Kling and Runway
  first for an existing subscriber, or Higgsfield as the default.
- **D2. Installs:** a visible maiTerm tab running brew (recommended: the human sees and can stop
  it, and Homebrew's own installer needs their password), or a silent background install.
- **D3. Music and sound effects:** ElevenLabs' hosted MCP documents only speech. The options are:
  - fal's music and sfx models (no extra account; checked at build);
  - ElevenLabs' API key through phase 5;
  - leave music and sfx out of v1.

  Recommended: fal first, ElevenLabs music after phase 5.
- **D4. Scope of the Hive:** phases 1–4 as one Hive with phase 5 later (recommended), or all five.
