import { describe, expect, it } from 'vitest';
import { compactLine, coverNames, createPickTracker, createSetupWatch, flippedIds, newlyReady, picksAfterRedetect, readyToolIds, isSetupOnly, pickerTools, createLatest, modelsLine, recommendedSetupNote, creditsLine, externalUrl, goalNamesMedia, groupRows, shouldStartOpen, stateLabel, stateTone, subscriberProviders, summaryLine, toggleId, toolLabel, unverifiedNote } from './media';
import type { HiveMediaCatalogue, HiveMediaProvider, HiveMediaRow } from '$lib/tauri/types';

const prov = (id: string, over: Partial<HiveMediaProvider> = {}): HiveMediaProvider => ({
  id,
  name: id,
  capabilities: [],
  models_note: '',
  capability_models: {},
  credits: 'api',
  route_kind: 'remote_mcp',
  auth: 'oauth',
  route_url: '',
  installable: false,
  quote_tool: '',
  retention: '',
  signup_url: '',
  billing_url: '',
  guide: [],
  subscriber: false,
  checked: '2026-11-03',
  unverified: false,
  unverified_capabilities: [],
  ...over,
});

const catalogue: HiveMediaCatalogue = {
  capabilities: [
    { id: 'video', label: 'Video', means: 'Text or image to video', common: true },
    { id: 'image', label: 'Images', means: 'Text to image', common: true },
    { id: 'voice', label: 'Voice', means: 'Text to speech', common: true },
    { id: 'music', label: 'Music', means: 'Music', common: true },
    { id: 'finish', label: 'Finishing', means: 'Cut, convert, mix', common: true },
    { id: 'lipsync', label: 'Lipsync', means: 'Talking avatars', common: false },
  ],
  providers: [
    prov('fal', { name: 'fal', quote_tool: 'get_pricing' }),
    prov('higgsfield', { name: 'Higgsfield', credits: 'plan', subscriber: true }),
    prov('elevenlabs', { name: 'ElevenLabs', credits: 'plan' }),
    prov('homebrew-kit', { name: 'Homebrew kit', credits: 'free', route_kind: 'brew', auth: 'none' }),
  ],
};

const row = (capability: string, over: Partial<HiveMediaRow> = {}): HiveMediaRow => ({ capability, covered_by: [], recommended: null, alternatives: [], ...over });

describe('stateLabel', () => {
  it('says the four states in maiTerm\'s words', () => {
    expect(stateLabel('ready')).toBe('Ready');
    expect(stateLabel('needs_sign_in')).toBe('Needs sign-in');
    expect(stateLabel('unreachable')).toBe('Not reachable');
    expect(stateLabel('installed')).toBe('Installed');
  });
  it('never throws on a state Rust adds later', () => {
    expect(stateLabel('hibernating')).toBe('Found');
    expect(stateLabel('')).toBe('Found');
    expect(stateLabel('constructor')).toBe('Found');
    expect(stateLabel('__proto__')).toBe('Found');
  });
});

describe('creditsLine', () => {
  it('api is prepaid credit, with the pricing tool when there is one', () => {
    expect(creditsLine({ name: 'fal', credits: 'api', quote_tool: '' })).toBe('Prepaid API credit');
    expect(creditsLine({ name: 'fal', credits: 'api', quote_tool: 'get_pricing' })).toBe('Prepaid API credit · prices a job before running it');
  });
  it('plan spends the named plan\'s credits', () => {
    expect(creditsLine({ name: 'Higgsfield', credits: 'plan', quote_tool: '' })).toBe("Spends your Higgsfield plan's credits");
    expect(creditsLine({ name: 'Runway', credits: 'plan', quote_tool: 'q' })).toBe("Spends your Runway plan's credits · prices a job before running it");
  });
  it('free and none are free', () => {
    expect(creditsLine({ name: 'x', credits: 'free', quote_tool: '' })).toBe('Free');
    expect(creditsLine({ name: 'x', credits: 'none', quote_tool: '' })).toBe('Free');
  });
  it('still says the money for the two odd kinds the catalogue holds', () => {
    expect(creditsLine({ name: 'Hedra', credits: 'plan_or_wallet', quote_tool: '' })).toBe("Spends your Hedra plan's credits or wallet balance");
    expect(creditsLine({ name: 'Comfy Cloud', credits: 'comfy_credits', quote_tool: '' })).toBe('Spends your Comfy Cloud credits');
  });
  it('omits a kind it does not know, and a provider with no route', () => {
    expect(creditsLine({ name: 'x', credits: 'barter', quote_tool: 'q' })).toBeNull();
    expect(creditsLine({ name: 'x', credits: '', quote_tool: '' })).toBeNull();
  });
});

describe('toolLabel', () => {
  it('drops the kind prefix', () => {
    expect(toolLabel('mcp:fal-mirror')).toBe('fal-mirror');
    expect(toolLabel('cli:ffmpeg')).toBe('ffmpeg');
    expect(toolLabel('plugin:remotion@remotion')).toBe('remotion@remotion');
    expect(toolLabel('comfyui')).toBe('comfyui');
    expect(toolLabel('mcp:')).toBe('mcp:');
  });
});

describe('groupRows', () => {
  const rows: HiveMediaRow[] = [
    row('video', { recommended: 'fal', alternatives: ['higgsfield'] }),
    row('image', { covered_by: [{ tool_id: 'mcp:fal', provider: 'fal', state: 'ready' }] }),
    row('voice', { covered_by: [{ tool_id: 'mcp:eleven', provider: 'elevenlabs', state: 'needs_sign_in' }], recommended: 'elevenlabs' }),
    row('music'),
    row('finish', { covered_by: [{ tool_id: 'cli:ffmpeg', provider: 'homebrew-kit', state: 'ready' }] }),
    row('lipsync', { recommended: 'fal' }),
  ];

  it('splits the common five from More, in catalogue order', () => {
    const g = groupRows(rows, catalogue);
    expect(g.common.map((r) => r.capability)).toEqual(['video', 'image', 'voice', 'music', 'finish']);
    expect(g.more.map((r) => r.capability)).toEqual(['lipsync']);
  });

  it('puts rows in the catalogue\'s order even when Rust sent them in another', () => {
    const g = groupRows([...rows].reverse(), catalogue);
    expect(g.common.map((r) => r.capability)).toEqual(['video', 'image', 'voice', 'music', 'finish']);
  });

  it('joins a cover with its provider name and state label', () => {
    const g = groupRows(rows, catalogue);
    const voice = g.common[2];
    expect(voice.label).toBe('Voice');
    expect(voice.means).toBe('Text to speech');
    expect(voice.covers).toEqual([
      { toolId: 'mcp:eleven', toolLabel: 'eleven', providerId: 'elevenlabs', providerName: 'ElevenLabs', state: 'needs_sign_in', stateLabel: 'Needs sign-in' },
    ]);
    expect(voice.ready).toBe(false);
    expect(g.common[1].ready).toBe(true);
  });

  it('joins the recommendation and alternatives with their credits lines', () => {
    const video = groupRows(rows, catalogue).common[0];
    expect(video.recommended?.name).toBe('fal');
    expect(video.recommended?.creditsLine).toBe('Prepaid API credit · prices a job before running it');
    expect(video.alternatives.map((a) => [a.id, a.creditsLine, a.subscriber])).toEqual([['higgsfield', "Spends your Higgsfield plan's credits", true]]);
    expect(groupRows(rows, catalogue).common[3].recommended).toBeNull();
  });

  it('is only ready for a ready cover, not a sign-in or an unreachable one', () => {
    const g = groupRows(
      [row('video', { covered_by: [{ tool_id: 'mcp:a', provider: 'fal', state: 'unreachable' }, { tool_id: 'mcp:b', provider: 'fal', state: 'installed' }] })],
      catalogue,
    );
    expect(g.common[0].ready).toBe(false);
  });

  it('survives what a newer Rust build sends: an unknown provider, state and capability', () => {
    const g = groupRows(
      [
        row('video', { covered_by: [{ tool_id: 'mcp:mystery', provider: 'mystery', state: 'dozing' }], recommended: 'mystery', alternatives: ['mystery', 'fal'] }),
        row('hologram', { recommended: 'fal' }),
      ],
      catalogue,
    );
    const video = g.common[0];
    expect(video.covers[0]).toMatchObject({ providerName: 'mystery', stateLabel: 'Found' });
    expect(video.recommended).toBeNull();
    expect(video.alternatives.map((a) => a.id)).toEqual(['fal']);
    expect(g.more.map((r) => [r.capability, r.label, r.common])).toEqual([['hologram', 'hologram', false]]);
  });

  it('answers empty groups for no rows', () => {
    expect(groupRows([], catalogue)).toEqual({ common: [], more: [] });
  });
});

describe('summaryLine', () => {
  const ready = (cap: string): HiveMediaRow => row(cap, { covered_by: [{ tool_id: 'mcp:x', provider: 'fal', state: 'ready' }] });
  it('folds the common five to a line', () => {
    const g = groupRows([ready('video'), ready('image'), row('voice'), row('music'), row('finish', { covered_by: [{ tool_id: 'cli:ffmpeg', provider: 'homebrew-kit', state: 'needs_sign_in' }] })], catalogue);
    expect(summaryLine(g)).toBe('Media: video, image ready · voice, music, finish not set up');
  });
  it('says only what is true when all or none are ready', () => {
    expect(summaryLine(groupRows(['video', 'image', 'voice', 'music', 'finish'].map(ready), catalogue))).toBe('Media: video, image, voice, music, finish ready');
    expect(summaryLine(groupRows(['video', 'image', 'voice', 'music', 'finish'].map((c) => row(c)), catalogue))).toBe('Media: video, image, voice, music, finish not set up');
  });
  it('ignores More', () => {
    const g = groupRows([ready('video'), ready('lipsync')], catalogue);
    expect(summaryLine(g)).toBe('Media: video ready');
  });
  it('has a line even with nothing to show', () => {
    expect(summaryLine({ common: [], more: [] })).toBe('Media: nothing to show');
  });
});

describe('goalNamesMedia', () => {
  it.each([
    'Make a 30 second video for the launch',
    'Design a LOGO and a thumbnail',
    'Record a voiceover and a voice-over take',
    'Compose a soundtrack with sound effects',
    'Add sfx and a podcast intro',
    'Generate imagery for the landing page',
    'Render the 3D scene',
    'Make 3D models',
    'Four ads for the campaign',
    'Animate the mascot: a short animation',
    'Photos, pictures and illustrations',
    'Cut a trailer',
    'Make an ad',
    'Make a movie',
    'Write a song',
    'Animated explainer',
    'Narrate the tutorial',
    'Make a GIF',
    'Cut a film, a reel and a clip',
    'Dub the clip into French; it was dubbed twice',
    'Design a poster and a banner',
    'Animate the logo',
    'Narrated walkthrough',
  ])('names media: %s', (goal) => {
    expect(goalNamesMedia(goal)).toBe(true);
  });

  it.each([
    'Fix the login redirect',
    'Add pagination to the users table',
    'Advance the schedule and add a header',
    'Upgrade the dependencies, adding retries',
    'Refactor the mail broadcast module',
    'Improve test coverage',
    'Reduce re-render churn in the sidebar',
    'Stop the component from re-rendering on every keypress',
    'Fix the imagemagick path',
    '',
  ])('does not: %s', (goal) => {
    expect(goalNamesMedia(goal)).toBe(false);
  });

  it('matches whole words, not parts of them', () => {
    expect(goalNamesMedia('The word is imaginary')).toBe(false);
    expect(goalNamesMedia('radio dispatch')).toBe(false);
    expect(goalNamesMedia('ad-hoc script')).toBe(true); // a hyphen is a word boundary: "ad" is a word here
  });

  it('is case-insensitive', () => {
    expect(goalNamesMedia('VIDEO')).toBe(true);
    expect(goalNamesMedia('Imagery')).toBe(true);
  });

  it('still catches a render of media beside the code word', () => {
    expect(goalNamesMedia('re-render the video')).toBe(true);
    expect(goalNamesMedia('Render a poster, not a re-render')).toBe(true);
  });
});

describe('shouldStartOpen', () => {
  const git = { kind: 'git' };
  const plain = { kind: 'plain' };

  it('opens when the goal names media, even in a repo', () => {
    expect(shouldStartOpen('Make a promo video', [git])).toBe(true);
  });
  it('opens when a capability was asked for up front', () => {
    expect(shouldStartOpen('Refactor the parser', [git], ['video'])).toBe(true);
  });
  it('stays folded for a code goal in a repo', () => {
    expect(shouldStartOpen('Fix the login redirect', [git])).toBe(false);
    expect(shouldStartOpen('Fix the login redirect', [git, plain])).toBe(false);
    expect(shouldStartOpen('Fix the login redirect', [git], [])).toBe(false);
  });
  it('opens when no folder is a repo', () => {
    expect(shouldStartOpen('Tidy the notes', [plain])).toBe(true);
    expect(shouldStartOpen('Tidy the notes', [plain, plain])).toBe(true);
    expect(shouldStartOpen('Tidy the notes', [])).toBe(true);
  });
  it('treats a folder kind it does not know as not a repo', () => {
    expect(shouldStartOpen('Tidy', [{ kind: 'svn' }])).toBe(true);
  });
  it('opens for "imagery" and folds for a UI re-render in a repo', () => {
    expect(shouldStartOpen('Improve the imagery', [git])).toBe(true);
    expect(shouldStartOpen('Cut the re-render cost of the table', [git])).toBe(false);
  });
  it('decided: a code word beside a media word opens', () => {
    expect(shouldStartOpen('Fix the video player bug', [git])).toBe(true);
  });
});

describe('subscriberProviders', () => {
  it('lists the subscriber providers in catalogue order', () => {
    expect(subscriberProviders(catalogue).map((p) => p.id)).toEqual(['higgsfield']);
    expect(subscriberProviders({ capabilities: [], providers: [] })).toEqual([]);
  });
});

describe('toggleId', () => {
  it('adds, removes, and never mutates', () => {
    const a = ['kling'];
    expect(toggleId(a, 'runway')).toEqual(['kling', 'runway']);
    expect(toggleId(['kling', 'runway'], 'kling')).toEqual(['runway']);
    expect(a).toEqual(['kling']);
  });
});

describe('unverifiedNote', () => {
  const base = { unverified: false, unverified_capabilities: [] as string[], checked: '2026-11-03' };
  it('is null for a confirmed provider', () => {
    expect(unverifiedNote(base)).toBeNull();
    expect(unverifiedNote(base, 'video')).toBeNull();
  });
  it('dates a provider flagged as a whole', () => {
    expect(unverifiedNote({ ...base, unverified: true })).toBe('unverified as of 2026-11-03');
  });
  it('flags only the capability named', () => {
    const p = { ...base, unverified_capabilities: ['music'] };
    expect(unverifiedNote(p, 'music')).toBe('unverified as of 2026-11-03');
    expect(unverifiedNote(p, 'video')).toBeNull();
    expect(unverifiedNote(p)).toBeNull();
  });
  it("flags fal's music and sfx cards only, not its confirmed video, image, lipsync, upscale or 3d", () => {
    const fal = { unverified: true, unverified_capabilities: ['music', 'sfx'], checked: '2026-11-03' };
    for (const cap of ['video', 'image', 'lipsync', 'upscale', '3d']) expect(unverifiedNote(fal, cap)).toBeNull();
    expect(unverifiedNote(fal, 'music')).toBe('unverified as of 2026-11-03');
    expect(unverifiedNote(fal, 'sfx')).toBe('unverified as of 2026-11-03');
  });
  it('flags every capability of a provider unverified as a whole (no per-capability list)', () => {
    const p = { unverified: true, unverified_capabilities: [] as string[], checked: '2026-11-03' };
    expect(unverifiedNote(p, 'video')).toBe('unverified as of 2026-11-03');
  });
  it('says plain "unverified" with no date', () => {
    expect(unverifiedNote({ ...base, unverified: true, checked: '' })).toBe('unverified');
  });
});

describe('externalUrl', () => {
  it('passes http(s) and refuses anything else', () => {
    expect(externalUrl('https://fal.ai/login')).toBe('https://fal.ai/login');
    expect(externalUrl('http://localhost:8188')).toBe('http://localhost:8188/');
    expect(externalUrl('javascript:alert(1)')).toBeNull();
    expect(externalUrl('file:///etc/passwd')).toBeNull();
    expect(externalUrl('not a url')).toBeNull();
    expect(externalUrl('')).toBeNull();
  });
});

describe('createLatest', () => {
  it('marks an earlier request stale once a later one starts', () => {
    const l = createLatest();
    const first = l.next();
    expect(first()).toBe(false);
    const second = l.next();
    expect(first()).toBe(true);
    expect(second()).toBe(false);
  });
  it('marks everything stale after cancel', () => {
    const l = createLatest();
    const r = l.next();
    l.cancel();
    expect(r()).toBe(true);
  });
});

describe('stateTone', () => {
  it('draws ready as ok, blocked states as warn, the rest neutral', () => {
    expect(stateTone('ready')).toBe('ok');
    expect(stateTone('needs_sign_in')).toBe('warn');
    expect(stateTone('unreachable')).toBe('warn');
    expect(stateTone('installed')).toBe('neutral');
    expect(stateTone('something_new')).toBe('neutral');
  });
});

describe('recommendedSetupNote', () => {
  const cover = (providerId: string, state: string) => ({ toolId: `mcp:${providerId}`, toolLabel: providerId, providerId, providerName: providerId, state, stateLabel: state });
  const rec = { id: 'higgsfield' } as never;
  it('says an unreachable recommended provider is set up but not reachable, naming no cause', () => {
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('higgsfield', 'unreachable')] })).toBe('Set up, but not reachable from here: check Preferences → MCP servers');
  });
  it('says one waiting for a sign-in is set up and needs it', () => {
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('higgsfield', 'needs_sign_in')] })).toBe('Set up, needs sign-in');
  });
  it('lets the sign-in win when the provider has covers in both states, in either order', () => {
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('higgsfield', 'needs_sign_in'), cover('higgsfield', 'unreachable')] })).toBe('Set up, needs sign-in');
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('higgsfield', 'unreachable'), cover('higgsfield', 'needs_sign_in')] })).toBe('Set up, needs sign-in');
  });
  it('is null when the recommendation is not on the row, or its cover is in another state', () => {
    expect(recommendedSetupNote({ recommended: rec, covers: [] })).toBeNull();
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('fal', 'unreachable')] })).toBeNull();
    expect(recommendedSetupNote({ recommended: rec, covers: [cover('higgsfield', 'installed')] })).toBeNull();
    expect(recommendedSetupNote({ recommended: null, covers: [cover('higgsfield', 'unreachable')] })).toBeNull();
  });
});

describe('modelsLine', () => {
  const fal = prov('fal', { models_note: 'All the models', capability_models: { video: 'Kling 4.0', music: ' Music models ' } });
  it("uses the capability's own line when there is one", () => {
    expect(modelsLine(fal, 'video')).toBe('Kling 4.0');
    expect(modelsLine(fal, 'music')).toBe('Music models');
  });
  it('falls back to models_note for a capability with no line, or an empty one', () => {
    expect(modelsLine(fal, 'image')).toBe('All the models');
    expect(modelsLine({ ...fal, capability_models: { video: '  ' } }, 'video')).toBe('All the models');
  });
  it('tolerates a provider without the map and without a note', () => {
    expect(modelsLine({ models_note: 'x' } as never, 'video')).toBe('x');
    expect(modelsLine(prov('wavespeed'), 'video')).toBe('');
  });
});

describe('coverNames', () => {
  it('says a name once when the tool and the provider are the same word, in any case', () => {
    expect(coverNames({ toolLabel: 'fal', providerName: 'fal' })).toEqual({ tool: 'fal', by: null });
    expect(coverNames({ toolLabel: 'ideogram', providerName: 'Ideogram' })).toEqual({ tool: 'Ideogram', by: null });
  });
  it('keeps both when they differ', () => {
    expect(coverNames({ toolLabel: 'fal-mirror', providerName: 'fal' })).toEqual({ tool: 'fal-mirror', by: 'fal' });
  });
});

describe('pickerTools', () => {
  const t = (id: string) => ({ id, kind: id.split(':')[0], label: id, host: null });
  it('leaves cli:brew out and keeps everything else, in order', () => {
    expect(pickerTools([t('cli:ffmpeg'), t('cli:brew'), t('mcp:fal')]).map((x) => x.id)).toEqual(['cli:ffmpeg', 'mcp:fal']);
  });
  it('does not touch a tool that merely mentions brew, and takes an empty list', () => {
    expect(pickerTools([t('mcp:brew-kit')]).map((x) => x.id)).toEqual(['mcp:brew-kit']);
    expect(pickerTools([])).toEqual([]);
  });
});

describe('isSetupOnly', () => {
  it('names the setup-only program, exactly', () => {
    expect(isSetupOnly('cli:brew')).toBe(true);
    expect(isSetupOnly('cli:ffmpeg')).toBe(false);
    expect(isSetupOnly('mcp:brew-kit')).toBe(false);
  });
});

describe('compactLine', () => {
  const rec = prov('fal', { name: 'fal' });
  const cover = (state: string) => ({ toolId: 'mcp:fal', toolLabel: 'fal', providerId: 'fal', providerName: 'fal', state, stateLabel: state });
  it('a ready row has covers to list, no status, no suggestion and nothing behind a disclosure', () => {
    expect(compactLine({ covers: [cover('ready')], ready: true, recommended: null })).toEqual({ covered: true, status: '', suggested: null, details: false, detailsLabel: '' });
  });
  it('an uncovered row says Not set up and suggests the recommendation, with a how-to behind it', () => {
    expect(compactLine({ covers: [], ready: false, recommended: { ...rec, creditsLine: null } })).toEqual({ covered: false, status: 'Not set up', suggested: 'Suggested: fal', details: true, detailsLabel: 'How to set up fal' });
  });
  it('an uncovered row with no recommendation says only Not set up, with nothing to open', () => {
    expect(compactLine({ covers: [], ready: false, recommended: null })).toEqual({ covered: false, status: 'Not set up', suggested: null, details: false, detailsLabel: '' });
  });
  it('a covered row that is not ready keeps its covers and offers Details when a provider is recommended', () => {
    const c = compactLine({ covers: [cover('needs_sign_in')], ready: false, recommended: { ...rec, creditsLine: null } });
    expect(c).toMatchObject({ covered: true, status: '', suggested: null, details: true, detailsLabel: 'Details: fal' });
  });
});

describe('after a setup', () => {
  const row = (capability: string, covers: [string, string][]): HiveMediaRow => ({ capability, covered_by: covers.map(([tool_id, state]) => ({ tool_id, provider: 'p', state })), recommended: null, alternatives: [] });
  const det = (...ids: string[]) => ids.map((id) => ({ id }));

  it('reads the ready tools once each, and what a setup newly made ready', () => {
    const rows = [row('video', [['mcp:fal', 'ready'], ['mcp:kling', 'needs_sign_in']]), row('image', [['mcp:fal', 'ready']])];
    expect(readyToolIds(rows)).toEqual(['mcp:fal']);
    expect(readyToolIds(null)).toEqual([]);
    expect(newlyReady(['cli:ffmpeg'], ['cli:ffmpeg', 'mcp:fal'])).toEqual(['mcp:fal']);
  });

  it('ticks a tool the setup made ready', () => {
    const next = picksAfterRedetect(new Set(['cli:ffmpeg']), det('cli:ffmpeg', 'mcp:fal'), ['mcp:fal'], new Set());
    expect([...next].sort()).toEqual(['cli:ffmpeg', 'mcp:fal']);
  });

  it('never overrides the human: a tool they unticked stays unticked, one they ticked stays ticked', () => {
    const unticked = picksAfterRedetect(new Set(['cli:ffmpeg']), det('cli:ffmpeg', 'mcp:fal'), ['mcp:fal'], new Set(['mcp:fal']));
    expect([...unticked]).toEqual(['cli:ffmpeg']);
    const kept = picksAfterRedetect(new Set(['cli:sox']), det('cli:sox'), [], new Set(['cli:sox']));
    expect([...kept]).toEqual(['cli:sox']);
  });

  it('leaves alone tools that were already ready, drops a pick no longer detected, and never ticks plumbing', () => {
    const next = picksAfterRedetect(new Set(['cli:gone']), det('mcp:old', 'cli:brew'), ['cli:brew', 'mcp:missing'], new Set());
    expect([...next]).toEqual([]);
  });

  it('says which ticks a picker change flipped', () => {
    expect(flippedIds(new Set(['a', 'b']), new Set(['b', 'c'])).sort()).toEqual(['a', 'c']);
  });
});

describe('createPickTracker', () => {
  const det = (...ids: string[]) => ids.map((id) => ({ id, kind: 'mcp', label: id, host: null }));
  const defaults = (t: readonly { id: string }[]) => new Set(['cli:ffmpeg', 'cli:gone'].filter((id) => t.some((x) => x.id === id)));

  it('keeps an earlier notice when a later, empty one overtakes it', () => {
    const tr = createPickTracker(defaults);
    tr.notice([]);
    expect([...tr.answered(new Set(), det('cli:ffmpeg'), new Set())]).toEqual(['cli:ffmpeg']);
    // Two overlapping notices: fal became ready, then a second Verify found nothing new.
    tr.notice(['mcp:fal']);
    tr.notice([]);
    // Only the newest request's answer is applied, and it still ticks fal.
    const next = tr.answered(new Set(['cli:ffmpeg']), det('cli:ffmpeg', 'mcp:fal'), new Set());
    expect([...next].sort()).toEqual(['cli:ffmpeg', 'mcp:fal']);
    // Consumed: a later answer doesn't tick it again after the human unticks it.
    expect([...tr.answered(new Set(['cli:ffmpeg']), det('cli:ffmpeg', 'mcp:fal'), new Set())]).toEqual(['cli:ffmpeg']);
  });

  it('applies the first-detection rule to the first answer that succeeds, after a failure', () => {
    const tr = createPickTracker(defaults);
    // The first detection failed (nothing applied), then a setup made fal ready.
    expect(tr.applied).toBe(false);
    tr.notice(['mcp:fal']);
    const next = tr.answered(new Set(), det('cli:ffmpeg', 'mcp:fal'), new Set());
    expect([...next].sort()).toEqual(['cli:ffmpeg', 'mcp:fal']);
    expect(tr.applied).toBe(true);
  });

  it('never ticks what the human flipped', () => {
    const tr = createPickTracker(() => new Set());
    tr.notice(['mcp:fal']);
    expect([...tr.answered(new Set(), det('mcp:fal'), new Set(['mcp:fal']))]).toEqual([]);
  });
});

describe('createSetupWatch', () => {
  const row = (capability: string, covers: [string, string, string][]): HiveMediaRow => ({ capability, covered_by: covers.map(([tool_id, provider, state]) => ({ tool_id, provider, state })), recommended: null, alternatives: [] });

  it("ignores another window's or provider's install, and answers its own once", () => {
    const w = createSetupWatch();
    w.started({ provider_id: 'homebrew-kit', tab_id: 't1' });
    expect(w.installEnded({ tab_id: 't-other-window' })).toBe(false);
    expect(w.installEnded({ tab_id: 't1' })).toBe(true);
    expect(w.installEnded({ tab_id: 't1' })).toBe(false);
  });

  it('hears only its own server connect, once', () => {
    const w = createSetupWatch();
    w.started({ provider_id: 'fal', server_id: 'fal-2' });
    expect(w.serverStatus({ id: 'linear', status: { state: 'connected' } })).toBe(false);
    expect(w.serverStatus({ id: 'fal-2', status: { state: 'needs_sign_in' } })).toBe(false);
    expect(w.serverStatus({ id: 'fal-2', status: { state: 'connected' } })).toBe(true);
    expect(w.serverStatus({ id: 'fal-2', status: { state: 'connected' } })).toBe(false);
  });

  it('with the tracker: an install that ends after the panel is gone ticks its tool, and only its tool', () => {
    // The Create sheet's flow: Set up the kit, close the sheet, Next to Stack (the panel unmounts),
    // the install exits 0 in its tab. The sheet's own watcher hears it and re-detects.
    const w = createSetupWatch();
    const tr = createPickTracker(() => new Set(['cli:ffmpeg']));
    const det = (...ids: string[]) => ids.map((id) => ({ id, kind: 'cli', label: id, host: null }));
    expect([...tr.answered(new Set(), det('cli:ffmpeg', 'mcp:fal'), new Set())]).toEqual(['cli:ffmpeg']);
    w.started({ provider_id: 'homebrew-kit', tab_id: 't1' });
    expect(w.installEnded({ tab_id: 't1' })).toBe(true);
    // Status after the install: whisper is ready through the kit; fal is ready too, set up elsewhere.
    const rows = [row('transcribe', [['cli:whisper-cli', 'homebrew-kit', 'ready']]), row('video', [['mcp:fal', 'fal', 'ready']])];
    tr.notice(w.readyFrom(rows));
    const picked = tr.answered(new Set(['cli:ffmpeg']), det('cli:ffmpeg', 'cli:whisper-cli', 'mcp:fal'), new Set());
    expect([...picked].sort()).toEqual(['cli:ffmpeg', 'cli:whisper-cli']);
  });
});
