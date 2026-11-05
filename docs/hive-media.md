# Hive media setup: what it can make, and getting the rest

Design, 2026-11-03 (Darryl's direction; decisions in §12). Nothing here is built yet. It extends `docs/hive.md`
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
2. **No API key in a plaintext config that maiTerm writes.** Any key maiTerm takes goes into the
   keychain as the MCP proxy's bearer token (docs/mcp-proxy.md §2, stored by §5's rules). The
   recommended routes are all remote MCP servers with OAuth, so most setups involve no key at
   all. A provider reachable only by a local MCP server with an env key is `manual` until the
   proxy grows stdio upstreams (phase 5). maiTerm only *shows* that guide: the human writes the
   entry, and the guide says the key will sit in plain text in `.claude.json`, and that a Hive
   worker given that server gets a copy of the entry in its own `mcp.json` (written 0600,
   `settings::worker_mcp_config`).
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
    "kind": "remote_mcp",                // remote_mcp | stdio_mcp (phase 5) | brew | plugin | local_app | manual
    "url": "https://mcp.fal.ai/mcp-relay",
    "auth": "oauth",                     // oauth | token | none
    "credits": "api",                    // api (prepaid balance) | plan (subscription credits)
    "quote_tool": "get_pricing"          // a tool that prices a job before it runs, if any
  }],
  "result_hosts": ["fal.media", "v3.fal.media", "v3b.fal.media"],
  "api_hosts": ["fal.run", "queue.fal.run", "api.fal.ai"],
  "sandbox_hosts": ["fal.run", "queue.fal.run", "fal.media", "v3.fal.media"],  // phase 1: exactly today's KNOWN_GENERATOR_HOSTS set (§9)
  "unverified_hosts": [],
  "retention": "7 days or longer",       // how long a result URL lives: the brief's "save at once"
  "signup_url": "https://fal.ai/login", "billing_url": "https://fal.ai/dashboard/billing",
  "guide": ["Make a fal account (or sign in).", "Add credit: fal bills a prepaid balance.", "Back here, press Sign in."],
  "match": { "hosts": ["fal.ai", "fal.run", "fal.media"], "names": ["fal"] }
}
```

**`match.hosts` keeps every suffix today's table has.** For example, Kling's entry matches both
`klingai.com` (today's) and `kling.ai` (its MCP's host).

The starting entries, from the 2026-11-03 survey:

| Provider | Route | Auth | Credits | Default for |
|---|---|---|---|---|
| **fal** | remote MCP `mcp.fal.ai/mcp-relay` | OAuth | API (prepaid), pricing tool | **video, image, lipsync, upscale, 3d** |
| **ElevenLabs** | remote MCP `api.elevenlabs.io/v1/mcp` | OAuth | plan | **voice** (music and sfx: see D3) |
| **Homebrew kit** | `brew install ffmpeg imagemagick sox whisper-cpp` | none | free | **finish, transcribe** |
| **Remotion** | Claude Code plugin `remotion@remotion` | none | free | **compose**, **coordinator only** (workers load no user plugins or skills: `--setting-sources project`, docs/hive.md §6.7 "Workers have no skills"); a worker renders through `npx remotion render` in its shell or a `hiveRun` job, with the npm registry it already has |
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
| MiniMax, Meshy | local MCP with an env key | key | API | `stdio_mcp` from phase 5 (§12.1) |
| Google genmedia | local MCP, needs a GCP project and a bucket | GCP | API | **manual** |
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
- **MCP proxy servers** (`mcp_proxy` registry), each with:
  - its status;
  - its **upstream URL**;
  - its **workspace scope**. A server scoped away from the Hive's workspace answers "not enabled
    for this workspace", so it counts as `unreachable` for that Hive, never `ready`.

  A proxied entry in `.claude.json` points at `http://127.0.0.1:<port>/mcp/x/<id>` (`<id>-dev` in
  dev, `mcp_proxy/entries.rs`). So it is matched to a catalogue provider by its upstream URL,
  never by the local one. Today `is_generator_server` vouches for a proxied entry by its *name*
  only, because its 127.0.0.1 host matches no suffix. For **proxied** entries it switches to the
  upstream URL: a proxied server named "fal" that points elsewhere isn't trusted, and a proxied
  fal server named "video" is.
- **Native `.claude.json` entries**: matched by `match.hosts`, then `match.names`, as today. A
  native stdio entry still has only its name to vouch for it.
- **Installed Claude Code plugins**, in the one Claude config root the coordinator loads
  (`install::coordinator_config_root`, built on `launch::config_root_for`). That is the active
  managed Claude account's root, read through its links to `~/.claude`, else the human's
  exported `CLAUDE_CONFIG_DIR`, else `~/.claude`. A plugin anywhere else isn't reported: in an
  exported root a managed account shadows, or in a root of an account that is switched off or
  not active. The coordinator never loads those, and workers load no user plugin at all. Install
  writes to the same root (`install::plugin_root`), so install, Verify and launch can't
  disagree. Remotion is found here.
- **ComfyUI** on 127.0.0.1:8188 (as today), and the Comfy Desktop app's presence.

## 6. Setup routes

Each route is one button on a missing capability's recommendation card. The button opens a small
**setup sheet**: the guide's steps, the credits line, and the action.

**The setup sheet: built (2026-11-05).** `components/hive/HiveMediaSetupSheet.svelte`, its
decisions as pure functions in `lib/hive/mediaSetup.ts`. "Set up <provider>" sits on each
recommendation card of the panel (§7) and "Set up" beside each alternative, in all three
placements. What it does per route:
- **`remote_mcp`**: the human's add path, shared with Preferences → MCP servers and an approved
  `mcp_add` (`lib/mcp/addServer.ts`). A URL maiTerm already serves is reused (`sameServerUrl`,
  `mcp_proxy::same_url`'s rules); a new server is named after the provider id, suffixed (`fal-2`)
  past another server's id or a name a native entry holds as agents would see it. **Scope** is set
  right after the add, whether or not the sheet is still open: from a Hive (`workspaceId` set:
  Create sheet or Hive settings) only that workspace; from Preferences the human picks "Hive
  workspaces only" (the Hive workspaces there are now, every window: `list_move_targets` carries a
  `hive` flag) or "Every workspace", and with no Hive yet only the second. A reused server limited
  to other workspaces gains the Hive's; one offered everywhere, or one reused from Preferences,
  keeps its scope.
  - `oauth`: the probe runs first. A server that can't register maiTerm points at Preferences (it
    needs a client id); one that asks for a key turns the sheet into the `token` form; then the
    sign-in opens in the browser (http(s) only). Verify runs by itself on the
    `mcp-proxy-status` that says the server is connected, or on the human's Verify. The button
    reads "Sign in", as the guides say.
  - `token`: the provider's page, a password field, `mcp_server_add` with the token (or
    `mcp_server_set_token` on a reused token server), then Verify. The field is emptied on success;
    the key is never logged, toasted or put in a URL.
- **`brew` / `plugin`**: listens for `hive-media-install-finished` first, then
  `hiveMediaInstall(provider, avoidWorkspaceId)`. The workspace the sheet was opened for is never
  the tab's (the Create sheet's isn't a Hive yet, so the window's own rule would take it). From
  Preferences, Rust opens the tab in the terminal window the human was last in
  (`focused_app_window`); the event still comes back to Preferences. "Running in a tab: <line>"
  with Show the tab (not from Preferences), Verify on the event, Re-detect when the shell can't
  report its exit, Run it again after a failure. A Homebrew kit with `transcribe` shows the speech
  model's progress and a retry. A plugin with no confirmed source (Remotion) gets no install
  button: its guide describes a tab maiTerm won't open, so the sheet says the steps are the
  human's (`guideFor`) and offers Verify.
- **`local_app`**: the download page; Verify runs whenever the window comes back to the front.
- **`manual`** and any kind this build doesn't know: the guide, and Verify.

Verify's answer is the sheet's state line (`verifyView`: Ready, Needs sign-in, Needs credit, Not
reachable, Installed, Didn't work), with the billing page as a button. The sheet never runs a
generation, starts nothing from the phone and adds no maiLink route; after any step that may have
changed this Mac, the panel re-fetches.

After that fetch the panel calls its parent's `onchanged` with the tools it newly calls ready
(owed until a fetch lands, so a Re-detect that overtakes it doesn't lose it). Steps that end after
the sheet closed still get there: a server write reports itself when it lands, closing the sheet
fetches again, and a panel that opened a sheet hears `hive-media-install-finished` and a server's
`connected` itself. The Create sheet detects its tools again and ticks a newly ready one unless
the human ticked or unticked it themselves (`createPickTracker`: every notice's tools are kept
until an answer is applied, and the first answer that succeeds, even after a failed first
detection, starts from the defaults or the draft); Hive settings' tools editor detects again and
shows it as a tool to add, never adding it to `Hive.tools` without the human's Save. No polling.

| Route | What maiTerm does | What the human does |
|---|---|---|
| `remote_mcp` + `oauth` | Adds the server to the MCP proxy through the human's own add path (docs/mcp-proxy.md §7): the human clicked, so no request card, but with `mcp_add`'s checks at filing: a URL maiTerm already serves is reused, and a name a native entry holds gets a suffix. **Scope**: started from a Hive (Create sheet or Hive settings), the server is scoped to that Hive's workspace, so one fal setup doesn't load fal's tools into every Claude tab (§4.3 context cost); started from Preferences, the sheet asks "every workspace" or "Hive workspaces only". Then it opens the sign-in in the browser | Signs in; adds credit if the guide says so |
| `remote_mcp` + `token` | Opens the provider's key page; a paste field saves the key to the keychain as the proxy's bearer token | Makes the key, pastes it |
| `brew` | Opens a **visible maiTerm tab** running the one install command, then re-detects when it exits. With no Homebrew, the first step is Homebrew's own installer in the same tab, since it asks for the human's password. For whisper.cpp, maiTerm then downloads the default model (`ggml-base.en`) from huggingface.co into a maiTerm-owned models folder, outside any sandbox, and the brief names that path | Watches; types a password if asked |
| `plugin` | Runs `claude plugin marketplace add …` and `claude plugin install …` in a visible tab | Watches |
| `local_app` | Shows the download link and guide; re-detects when the window regains focus | Installs the app |
| `stdio_mcp` (phase 5) | maiTerm runs the pinned local server itself, with the key from the keychain (§12.1); setup is the `token` setup plus Verify | Makes the key, pastes it |
| `manual` | Shows the guide, including the config snippet with the key left as a placeholder | Everything |

**`brew` and `plugin`: built (2026-11-05).** The backend and the window's half, and the setup sheet
on top of them (above). Where they live: `hive/media/install.rs`, `hive/media/whisper.rs`
and `src/lib/hive/mediaInstall.ts`.
- **The line** (`install::plan`) comes from the catalogue route alone. The sheet sends a provider
  id, never a command, and every word is checked against a narrow alphabet. The pieces:
  - `brew`: the route's `brew install …`, and nothing else is accepted. It calls the `brew`
    found on this machine by its full path.
  - No Homebrew: Homebrew's own installer (`/bin/bash -c "$(curl … install.sh)"`) runs first,
    then `&&` and the new `brew` by its default prefix (`/opt/homebrew`, `/usr/local` or
    Linuxbrew), since the tab's shell has no `brew` on its PATH yet.
  - `plugin`: `env -u CLAUDE_CONFIG_DIR claude plugin marketplace add <marketplace>; … claude
    plugin install <plugin_id>`, or `env CLAUDE_CONFIG_DIR=<root>` in place of `env -u`. It
    installs into the root the coordinator loads (`install::coordinator_config_root`, the root
    §5's plugin detection reads, through `install::plugin_root`), whatever account the tab
    spawned under:
    - With a managed Claude account active, that is `~/.claude`, even when the human also
      exported a `CLAUDE_CONFIG_DIR`. The coordinator launches under the managed root, whose
      `plugins` and `settings.json` link back to `~/.claude`, never to the exported root. The
      links are re-made by `accounts::resync_roots` after a success.
    - Without one, it is the human's exported `CLAUDE_CONFIG_DIR`, else `~/.claude`.

    Workers launch with `--setting-sources project`, so no worker loads it.
  - A route's `marketplace` field (optional) is the source `marketplace add` takes. **Remotion
    has none**: no source is confirmed (neither remotion-dev repo has a `marketplace.json`). So
    its install is refused, in plain words that point at its guide, until one is.
  - Refused: an unknown provider, a provider with no `brew` or `plugin` route, Windows, and the
    same provider while it is already installing.
- **The tab.** `hive_media_install` sends `media.install.start` to the calling window
  (`hive-frontend-request`). The window opens a plain strip tab on THIS machine, never inherited
  from an ssh tab. It goes in the active workspace unless that is a Hive, the Overlord's or
  suspended, then the first workspace that takes one, then a new "Setup" workspace without
  switching to it. The window waits for the shell at a settled prompt (`stackStore.typeAtPrompt`:
  OSC 133 A with nothing begun since, or the tty-foreground probe for a shell without
  integration), arms the install in Rust, and types the line. It isn't an agent tab, so the
  agent-input guards don't apply. A refused arm (Rust gave up) closes the tab and nothing is
  typed.
- **The end.** The PTY reader hands every OSC 133 mark to `install::on_osc133`. The first B
  after the arm begins the line, and the D after it is its exit. Then
  `hive-media-install-finished {provider_id, tab_id, ok, exit_code, ended: exited|closed,
  model_download}` goes to the window, and the sheet re-detects. There is no polling. A tab whose
  shell ends first answers `closed`. A shell with no OSC 133 answers `watches_exit: false`, so
  the sheet re-detects on the human's word.
- **whisper.cpp's model.** After a Homebrew kit line that includes `whisper-cpp` exits 0,
  maiTerm downloads `ggml-base.en` from huggingface.co into `<app data>/models/whisper/`
  (`app_data_dir()`).
  - Pinned: its size (147964211 bytes) and SHA-256 (`a03779c8…6d002`) are constants
    (`whisper::MODEL_SIZE`, `MODEL_SHA256`). They were read from huggingface.co on 2026-11-05.
  - Idempotent: a model whose verified record (`ggml-base.en.bin.json`) names the pin, at that
    size, makes no request.
  - Resumable: `.part` plus a record of which file it is part of, continued with a Range. A part
    of a different file, a server ignoring the Range, or a refused range starts over.
  - The `resolve` URL answers a 302 to its CDN (`us.aws.cdn.hf.co`) with `x-linked-size` and a
    double-quoted `x-linked-etag`. maiTerm reads those from the 302 without following it, then
    fetches the bytes from its `location`. It runs outside every sandbox, so the CDN host needs no
    grant.
  - Published facts that disagree with the pin fail at once, and nothing is downloaded or kept.
    The check holds even if huggingface.co stops publishing them: the bytes received must hash
    to the pin, and a mismatch deletes the part.
  - Progress: `hive-media-model-progress` (`ModelStatus`) to every window.
  - `hive_media_whisper_model_path` names the file for the brief; it is null until the model is
    verified. `hive_media_whisper_model_download` is the sheet's retry.

**Verify.** After a route finishes, maiTerm re-detects and, for an MCP server, lists its tools
through the proxy (free). Where the provider has a balance or pricing tool, it calls that too:
fal `get_pricing`, WaveSpeed `get_balance`. The row turns **Ready**, or shows the error in plain
words ("Signed in, but fal answered 402: add credit at fal.ai/dashboard/billing", with the link
as a button). It never runs a paid generation.

**Verify: built (2026-11-05), `hive/media/verify.rs`.** `hive_media_verify(workspace_id?,
provider_id) → { state, message, link?, tool_count? }`, TS `hiveMediaVerify(workspaceId,
providerId)`. `state` is a String: `ready` | `needs_sign_in` | `needs_credit` | `unreachable` |
`error`, plus `installed` for something present that maiTerm can't check (a plugin switched off,
an app not running, the human's own MCP entry). For a provider served through the proxy (picked by
its upstream URL, preferring a server that is on and in the Hive's scope), in order:
- **On and in scope** for the Hive's workspace, else `unreachable` with what to change in
  Preferences → MCP servers.
- **The entry reaches agents.** The proxied entry must be written, provably ours
  (`entries::origin`), in `~/.claude.json` and every managed Claude account root that has a
  `.claude.json`. This catches phase 1's gap: with Claude Code IDE integration off, no entry is
  written and `hive_media_status` (which reads the registry) still says `ready`; Verify says
  `unreachable` and names the switch. A native entry holding the name, a stale port or a missing
  root are said in plain words too.
- **The tools list** through the proxy's own `refresh` (free). A failed read leaves the saved list
  as it was (docs/mcp-proxy.md §4.1).
- **The check call**, through `mcp_proxy::call_once` (its own session, never touching the saved
  list). Only `verify::CHECK_TOOLS` is ever called: fal `get_pricing`, WaveSpeed `get_balance`,
  BFL `get_credits`, Recraft `get_user`, Meshy `meshy_check_balance`. WaveSpeed's `get_price`
  needs a job's arguments and is never called. Arguments are only the ones the tool's schema
  requires, from a hard-coded harmless value (fal: one model's price), its `default` or its first
  `enum`; a required argument with none of those means no call.
- **Words.** A 401 or 403 is `needs_sign_in`. A 402, or a tool answering that the account is out
  of credit, is `needs_credit`, with `link` set to the catalogue's `billing_url` (http(s) only).
  Arguments the server rejects (`-32602`) are maiTerm's guess failing, so the result stays `ready`
  and says the account wasn't read.

Any other provider is re-detected with `status::hits` (the same sources as `hive_media_status`):
the Homebrew kit is `ready` only with every program it lists, and names the missing ones.

**Phase 5: stdio upstreams in the MCP proxy** (docs/mcp-proxy.md §2 lists them as v2). maiTerm
runs a local MCP server itself, with the key from the keychain in its environment. That moves
MiniMax and Meshy from `manual` to `stdio_mcp`, and keeps those keys out of `.claude.json`.
Specified in §12.1.

## 7. Where it appears

- **The Create sheet's Tools step**: a **What it can make** panel above the existing picker, which
  stays for everything else (code tools, skill packs). Detected media tools are ticked as today.
  The panel starts folded to one line ("Media: video, image ready · voice, music not set up") for
  a Hive whose goal names no media and that has a git folder, so a code Hive isn't slowed. It starts
  open when the goal names media, when a capability was asked for up front, or when no folder is
  a git repo. That is an obvious-default rule in code (`shouldStartOpen`: goal words, folder
  kinds), not an AI judgment, and it is read once, when the panel mounts: after that the human's
  fold wins.
  - **Open, it is compact**: a dim line saying it only reads, then one line per capability (its
    tool and state chips, or "Not set up" with "Suggested: <provider>"). A capability nothing
    ready covers has a button ("How to set up X", or "Details: X" when something covers it but
    isn't usable yet); the recommendation's card sits behind it. "I already pay for…" follows the
    common five, and the rest are under "More (n)".
  - **The card** holds that capability's models line, the credits line (§2.3), an "unverified as
    of <date>" note, the guide, Sign up and Billing links, and the alternatives.
- **Hive settings → Tools** for an existing Hive. **Built in phase 3 (2026-11-05)**:
  `hive/tool_change.rs`, `HiveToolsEditor.svelte`. Before it, `Hive.tools` was fixed at Create.
  It is a tools change for the human, a window command (`hive_set_tools`) and not an agent tool.
  It:
  - writes `Hive.tools`, every id checked against a fresh detection, with `cli:brew` and anything
    this Mac lacks refused;
  - offers the new tools' hosts, ticked, into the gallery hosts (the human's edit, as Re-detect
    does). They come from `generator_hosts` for the added tools only, shown before Save by
    `hive_tools_change_preview`;
  - sends the coordinator a framed notice;
  - takes effect for workers at their next launch;
  - is the Tools row in docs/hive.md §3.4.

  A dim "On this Mac" note above the media panel says the panel shows this computer's setup, not
  the Hive's grant.
- **Preferences → Hive**, above the default tools: the same panel, to set up a studio once for
  every future Hive. It is always folded here, under the note "On this Mac, for any Hive": open it
  is tall enough to push the defaults off the first screen. It has no workspace, so it judges no
  MCP server's scope.
- **The phone**: the status is visible. Only two setups can be started there:
  - a `brew` install when Homebrew is already present (a first install asks for a password at
    the Mac);
  - a `plugin` install.

  A key is never pasted on the phone (docs/mcp-proxy.md §8). An OAuth sign-in waits for the Mac
  (docs/mcp-proxy.md §7's stated exception).

## 8. Agents asking for a capability

A worker that needs a tool the Hive lacks already stops and reports it (the media roles say so).
That report becomes a structured ask:
- **`hiveRequestCapability {capability, why}`**, for the coordinator only. It files an agent
  request of a new kind, `tool_setup`, whose card is maiTerm's words: "The Hive *Launch video*
  needs **music**. Recommended: ElevenLabs (signs in with your ElevenLabs plan's credits). Set up…".
  The agent's `why` is quoted.
- **Approving opens the setup sheet** on the desktop. Hive kinds are carried out in Rust, so
  approval emits an event to the Hive's window, which opens the sheet. Approved from the phone,
  only §7's phone-startable setups run; the rest wait for the Mac.
- **One route, not two.** The coordinator also holds `addMcpServer`, which files `mcp_add`, so
  without a rule a missing remote-MCP capability could get either card. The rule:
  - the coordinator's brief and `addMcpServer`'s refusal for a Hive agent point it at
    `hiveRequestCapability` for a media capability;
  - `tool_setup` approval of a remote MCP goes through the same add (and `mcp_add`'s checks) as
    the setup sheet.
- **Plumbing it needs** (phase 4):
  - The tool goes in `COORDINATOR_TOOLS` (`hive/tools.rs`, a fixed-size array),
    `COORDINATOR_MCP_TOOLS` (`hive/settings.rs`), and `PEER_ADDRESSING_TOOLS` and
    `OWN_TAB_ONLY_TOOLS` (`claude_code/server.rs`), as `hiveChangeSettings` does.
  - The kind goes in `agent_requests.rs`'s kinds and `is_hive_kind`.
  - **Trap**: `hive_approved` routes every Hive kind that isn't `change::KIND` to
    `land::approval::on_approved`. `tool_setup` needs its own arm there, or its approval would
    be run as a land.
  - A new kind is drawn from the card's text fields, so it is **not** a maiLink protocol bump
    (docs/mailink-protocol.md, agent requests); bump only if the wire changes.
- When the capability becomes Ready, it is added to the Hive, and the coordinator gets a framed
  notice ("music is now available through elevenlabs; your workers launched from now on have it").
- `proposeHive` (§3.7) gains an optional `media: [capability]`, so a planning agent's proposal opens
  the sheet with those capabilities' panel unfolded.

## 9. Sandbox hosts

- **Every MCP server runs outside the sandbox**, proxied or native, in Claude's process (docs/hive.md
  §14.16, `settings.rs` "Generator calls run in Claude's process"). Only the worker's **shell** is
  allowlisted. So a worker needs:
  - `result_hosts`, to download with curl what a server returns;
  - `api_hosts` only when its shell calls the API itself (a REST call, or the whisper model
    fetch, which maiTerm does instead).
- **Phase 1 changes no hosts.** Each of today's five providers keeps exactly its current set as
  `sandbox_hosts`, so moving to the catalogue changes no behaviour. Narrowing to
  `result_hosts` (dropping `fal.run` and `queue.fal.run` from MCP-only workers) is a separate,
  later change, made once the result-host spike has confirmed the hosts.
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

The server names in the map are the ones in the worker's own `mcp.json`, for example `fal-dev`
in a dev build, never a hardcoded name.

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
   upstream-URL matching for proxied servers, scope-aware status, plugin detection per account
   root, and the host and worker-MCP code moved onto the catalogue. No hosts change (§9). Plus
   the result-host spike.

   **Phase 1, catalogue half: built (2026-11-03).** The catalogue, its loader, the host and
   worker-MCP code moved onto it, and proxied-entry matching by upstream URL. The status half is
   built too: `hive_media_status` (`hive/media/status.rs`), the extra CLIs (`ffprobe`,
   `whisper-cli` and `brew`, in `DETECTED_CLIS`, `hive/create.rs`), plugin detection per Claude
   config root (`plugin_hits`), and ComfyUI and Comfy Desktop detection (`comfy_hits`). Not built
   yet: the result-host spike. Where the catalogue half lives:
   - `resources/hive/media/catalogue.json` and `hive/media.rs`: `catalogue()`, `provider(id)`,
     `capability(id)`, `common_capabilities()`, `providers_for_host(host)`,
     `providers_for_upstream(url)`, `providers_for_native(name, host)`, `providers_for_skill(name)`.
     Beyond §4's fields an entry carries `offer` (`recommend` | `list` | `detect_only`),
     `subscriber_first` (D1), `recommendable`, `default_for`, `capability_notes`, `detect`
     (`clis`, `plugins`, `port`, `apps`: the recipes `hive_media_status` reads) and
     `match.skills`; a route has `command` (brew) and `plugin_id`. The parse test uses
     `deny_unknown_fields`, so a typo is a failing test.
   - `KNOWN_GENERATOR_HOSTS` and `SKILL_GENERATORS` are gone.
     `todays_five_providers_keep_exactly_their_hosts` pins fal, kie, higgsfield, kling and
     wavespeed to their old `sandbox_hosts` and suffixes; no other provider has a sandbox host.
   - A proxied entry is recognised by `mcp_proxy::entries::origin` (our marker AND exactly the url
     this launch writes: `http://127.0.0.1:<the bound port>/mcp/x/<entry name>`, no userinfo,
     query or other port) plus the registry's upstream URL (`DetectedTool.proxy`); anything else
     carrying the marker is recognised as nothing.
   - `generator_hosts` yields exactly the hosts (order aside) the old table's logic did for every native-server and
     skill input: `generator_hosts_equals_mains_for_every_native_and_skill_input` runs both (the
     old table and logic are copied into the test as an oracle) over a matrix of hosts and names.
     A provider's `sandbox_hosts` and a server's own host count only through `match.own_hosts`
     (the old suffixes; Kling's is `klingai.com` only) or a name word; `match.hosts` only
     recognises, so `kling.ai` and the providers new to the catalogue add no host.
   - A proxied entry has no old answer (main matched it by name). Its rule is the provider's
     `sandbox_hosts` when the upstream's host is under an `own_hosts` suffix, nothing otherwise;
     `a_proxied_entry_yields_only_hosts_the_old_five_had` proves that is a subset of the five
     providers' old hosts and independent of the entry's name.
   - **Verified 2026-11-04:** fal's music and sfx models (D3), from the model pages named in
     fal's `source_urls`; their UNVERIFIED `capability_notes` are gone. The same pass added the
     result hosts it found to `unverified_hosts` (BFL's `delivery.*.bfl.ai`, Runway, Meshy,
     Comfy Cloud: none is a sandbox host) and a route's optional `balance_tool` (a balance
     reader, which prices nothing, unlike `quote_tool`). Remotion's marketplace source is
     not in the catalogue (only `plugin_id`).
2. **The panel.** "What it can make" in the Tools step, Hive settings and Preferences, read-only:
   detection, recommendations, credits lines and guides.

   **Phase 2: built (2026-11-05).** Read-only: no setup button, and the panel emits nothing. Where
   it lives:
   - `components/hive/HiveMediaPanel.svelte`, with every join and wording as pure functions in
     `lib/hive/media.ts` (`groupRows`, `summaryLine`, `shouldStartOpen`, `creditsLine`,
     `modelsLine`, `unverifiedNote`, `compactLine`, `coverNames`, `pickerTools`, `isSetupOnly`).
     It reads two commands: `hive_media_status` (ids only) and `hive_media_catalogue`
     (`hive/media/view.rs`: the display facts, with no hosts, detection recipes or source URLs),
     and Re-detect fetches again.
   - **Three placements**, all §7's panel:
     - the Create sheet's Tools step (`HiveCreateSheet.svelte`), above the picker, open by
       `shouldStartOpen`; the picker's hint is now "Tick what this Hive's workers may use.";
     - Hive settings (`HiveSettingsModal.svelte`), a read-only Tools section with a "Workers get:"
       line, and the Tools row gone from "Fixed at creation";
     - Preferences → Hive (`HiveDefaultsSection.svelte`), folded, under "On this Mac, for any
       Hive".
   - **The open form is compact** (§7): one line per capability, the card behind a per-row button,
     a dim line at the top saying opening a card adds nothing to the Hive.
   - **`cli:brew` is setup plumbing.** Status needs to see Homebrew, but no worker makes media with
     it, so it is hidden from every tool picker (`pickerTools`) and shed from saved defaults
     (`normalizeHiveDefaults`, `defaultsPicked`).
   - **The catalogue gained** `capability_models` (a models line per capability, so a fal card
     under Music doesn't list the video models; `modelsLine` prefers it to `models_note`) and a
     route's `balance_tool`, which the panel doesn't show: phase 3's verify step reads it.
   - **Unverified is read from model notes only.** A provider is flagged unverified (as of its
     `checked` date) from its `models_note` or an UNVERIFIED `capability_notes` entry, and per
     capability when it has such notes. `unverified_hosts` never flags it: an unconfirmed result
     host is a sandbox matter (§9).
   - **The Hive settings tools change** came in phase 3 (§7, `hive/tool_change.rs`,
     `HiveToolsEditor.svelte`, below the panel in Hive settings' Tools section). The panel itself
     gained its "Set up" buttons with the setup sheet (§6).
3. **Setup routes and the tools change.**
   - `remote_mcp` (OAuth and token) through the proxy, scoped to the Hive;
   - `brew` (with the whisper model) and `plugin` in a visible tab (backend and window half built
     2026-11-05, §6), and the setup sheet over every route: **built 2026-11-05** (§6, "The setup
     sheet");
   - verification (built 2026-11-05: §6, "Verify: built");
   - the new after-Create tools change of §7 (a §3.4 row): **built 2026-11-05** (`hive/tool_change.rs`).
4. **Agents.**
   - the capability map in the briefs;
   - `hiveRequestCapability` + `tool_setup` requests, with every piece of plumbing §8 lists, on
     the Loom and the phone, no protocol bump;
   - `proposeHive`'s `media`.
5. **Stdio upstreams in the MCP proxy**, so MiniMax and Meshy become `stdio_mcp` (§12.1).

**These are phases of the design, not a queue.** What really depends on what:
- **Phase 1's catalogue and detection are the foundation.** Phases 2, 3 and 4 read
  `hive_media_status` and the catalogue's entries.
- **First, agree two types:** `hive_media_status`'s row, and the catalogue entry (name, credits,
  guide, routes). Agreeing them lets the rest be built alongside phase 1.
- **Independent of phase 1, and can start at once:**
  - **Phase 5's proxy side:** the stdio transport in `mcp_proxy/`, the `ProxyServer` stdio arm,
    the child lifecycle, the saved-list serving and the environment rules. Its catalogue route
    needs phase 1, and its key paste needs phase 3's sheet.
  - **Phase 4's registration:** the `tool_setup` kind, the tool's registration in the four tool
    lists, and the `hive_approved` arm. The card's words (recommended provider, credits) wait for
    phase 1. What approval does (open the setup sheet, add the tool once it is Ready) waits for
    phases 1 and 3.
  - **Phase 3's tools change after Create** (`Hive.tools`, with a §3.4 row). It offers hosts
    through the existing `generator_hosts`, which phase 1 later moves onto the catalogue.
  - **The result-host spike**, once Darryl approves its spend.
- **Phase 2's panel** can be built against the two agreed types before detection is finished.

Each piece is reviewed as it lands. The sheets are Svelte, and every sheet change so far had
ordering bugs that only review found.

## 12. Decisions (Darryl, 2026-11-03)

- **D1. The default video and image route is fal.** Higgsfield, Kling and Runway show first to
  someone who already subscribes to them.
- **D2. Installs run in a visible maiTerm tab.**
- **D3. Music and sound effects come from fal's models now,** and ElevenLabs music arrives with
  phase 5.
  - The build checks which fal models cover music and sfx, and writes them into the catalogue.
  - ElevenLabs' own local MCP server was archived in August 2026. So phase 5 confirms that a
    maintained stdio server (or the official CLI) covers music and sfx before offering it.
  - If none does, ElevenLabs stays speech-only and fal keeps music and sfx.
- **D4. All five phases, in one Hive.**

### 12.1 Phase 5, specified

Stdio upstreams in the MCP proxy (docs/mcp-proxy.md §2 "v2"), so a key-based local MCP server is
set up like a token one. Phase 5 also brings docs/mcp-proxy.md up to date.

- **The record.** `ProxyServer` gains a stdio arm that holds the catalogue route id, the
  command and the args. Today `ProxyServer.url` and `valid_url` require a URL
  (`mcp_proxy/mod.rs`). Detection (§5) and `is_generator_server` match a stdio server **by its
  catalogue route id**, since it has no upstream URL. Without that, a worker's `mcp.json` would
  leave it out.
- **Who runs it.** maiTerm starts the server as its own child process, from a catalogue route
  `{kind: "stdio_mcp", command, args, env_key}`.
  - **One child per server, shared by every downstream session.** HTTP upstreams get a session
    per agent (`mcp_proxy/mod.rs`); a stdio child has one session, so per-agent isolation is
    given up. That is acceptable for stateless generators, and the catalogue offers only those
    here. The proxy remaps JSON-RPC request ids so concurrent calls from different agents don't
    collide.
  - The command is pinned to an exact package version (`npx -y pkg@1.2.3`,
    `uvx pkg==1.2.3`). That doesn't pin the package's dependencies: say so on the setup sheet,
    or install it once into a maiTerm-owned folder from a lockfile and run it from there
    (preferred where the package ships one). The human's setup click is the approval to run that
    code as them.
  - It is started as `watch.rs` starts a script:
    - `env_clear`, then the login PATH (`login_path`), since a Finder-launched maiTerm has no
      `npx` on its PATH;
    - the key variable;
    - nothing else of maiTerm's environment;
    - a working directory in a maiTerm-owned folder.
  - **Lifecycle.**
    - The process is restarted on exit, with backoff.
    - It is stopped when no *tool call or tool list from an agent* has reached it for
      10 minutes. The proxy's own hourly re-list (`watch`) neither counts as use nor starts the
      process.
    - A stopped server answers `tools/list` from its **saved last-good list** (§4.1 "a failure
      never removes a tool") and shows as "idle", not as an outage.
    - The first `tools/list` happens during setup's Verify, before the entry is written into any
      config. So no agent ever sees an empty first list, even when `npx` takes longer than the
      proxy's 15 s wait.
  - It is served at `/mcp/x/<id>` like any proxied server. Agents never see the command or the
    key.
- **The key.** It is pasted in the setup sheet and stored in the keychain, never on the phone
  (§7). It reaches the child in its environment only, never in argv. That environment is
  inherited by whatever `npx` or `uvx` spawns: that is the package's own code, the same trust as
  the click. The key stays out of logs, errors and `Debug`, under `accounts::vault`'s rules. The
  child's **stderr is never logged or forwarded**, since a server may print its own key. Only a
  line count and the exit status are logged.
- **Sandbox and files.** The server runs outside every Hive sandbox, as every MCP server does
  (§9). A server that returns URLs is handled through the catalogue's `result_hosts`.
  - A server that writes files locally gets an output folder **outside maiTerm's data folder**,
    `~/Library/Caches/<app_data_slug>/mcp-out/<id>`, because `check_output_folders` refuses
    anything inside the data folder.
  - When the human adds that tool to a Hive (Create or the tools change), that folder is added
    to the Hive's output folders, ticked, as their edit. That is how `galleryAdd` accepts its
    files (docs/hive.md §10.3).
  - The folder is shared by every Hive using that server. As with any output folder, this
    prevents accidents, not a determined agent.
- **Providers it unlocks.**
  - MiniMax (speech, music, Hailuo video) and Meshy (3D);
  - ElevenLabs music and sfx, if D3's check passes.

  Google genmedia stays `manual`: it needs a GCP project and a bucket, not just a key.
- **The route list** gains `stdio_mcp`, alongside §4's kinds and with a row in §6's table. Its
  setup is the `token` setup (open the key page, paste) followed by Verify.
