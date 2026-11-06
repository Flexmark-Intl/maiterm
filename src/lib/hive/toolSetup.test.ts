import { describe, expect, it } from 'vitest';
import type { AgentRequest, HiveMediaCatalogue, HiveMediaProvider, HiveMediaRow } from '$lib/tauri/types';
import { createOpeningQueue, missedOpenings, openingFrom, parseApproved, seededSetup, type ToolSetupOpening } from './toolSetup';
import { isHiveRequest, unannouncedHiveCards, windowCarriesOut, windowTellsAsker } from './requests';

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
  stdio_routes: [],
  ...over,
});

const catalogue: HiveMediaCatalogue = {
  capabilities: [
    { id: 'voice', label: 'Voice', means: 'Text to speech', common: true },
    { id: 'music', label: 'Music', means: 'Music', common: true },
    { id: 'finish', label: 'Finishing', means: 'Cut, convert, mix', common: true },
  ],
  providers: [
    prov('elevenlabs', { name: 'ElevenLabs', credits: 'plan' }),
    prov('minimax', { name: 'MiniMax' }),
    prov('homebrew-kit', { name: 'Homebrew kit', credits: 'free', route_kind: 'brew', auth: 'none', installable: true }),
  ],
};

const row = (capability: string, over: Partial<HiveMediaRow> = {}): HiveMediaRow => ({ capability, covered_by: [], recommended: null, alternatives: [], ...over });

// The payload exactly as `tool_setup::approved_in` builds it.
const payload = (over: Record<string, unknown> = {}) => ({
  request_id: 'r_1',
  workspace_id: 'ws',
  hive_id: 'h',
  capability: 'music',
  provider_id: 'elevenlabs',
  by: 'desktop',
  install: null,
  waiting_for_mac: false,
  ...over,
});

const req = (over: Partial<AgentRequest>): AgentRequest => ({
  id: 'r_1',
  tab_id: 'coord',
  window_label: 'main',
  kind: 'tool_setup',
  args: { hive_id: 'h', workspace_id: 'ws', capability: 'voice', provider_id: 'elevenlabs', hosts: [] },
  summary: 'The Hive “Make it” needs voice (text to speech, voice design). Recommended: ElevenLabs (spends your ElevenLabs plan’s credits). Set up…',
  effects: ['Approving opens ElevenLabs’s setup sheet in this Hive’s window on the Mac: you sign in with ElevenLabs', 'Approved from your phone, it waits for you at the Mac.'],
  reason: 'The narration needs a voice.',
  asker_title: 'coordinator',
  asker_workspace: 'Make it',
  requested_at: 1_000,
  expires_at: 2_000,
  status: 'done',
  decided_at: 1_500,
  decided_by: 'phone',
  outcome: 'Approved, waiting for the Mac: signing in to ElevenLabs happens in a browser at the Mac. Its setup sheet is open there.',
  ...over,
});

describe('a tool_setup request in the window', () => {
  it('is a Hive request: Rust carries it out and tells the queen, so the window does neither', () => {
    expect(isHiveRequest({ kind: 'tool_setup' })).toBe(true);
    expect(windowCarriesOut({ kind: 'tool_setup', window_label: 'main' }, 'main')).toBe(false);
    expect(windowTellsAsker({ kind: 'tool_setup' })).toBe(false);
  });

  it('is announced once in the window that holds it, like the other Rust-filed cards', () => {
    const seen = new Set<string>();
    const pending = [req({ status: 'pending', decided_at: null, decided_by: null, outcome: null })];
    expect(unannouncedHiveCards(pending, seen).map((r) => r.id)).toEqual(['r_1']);
    expect(unannouncedHiveCards(pending, seen)).toEqual([]);
  });
});

describe('parseApproved', () => {
  it('reads Rust’s payload', () => {
    expect(parseApproved(payload())).toEqual(payload());
    const install = { tab_id: 't1', command_shown: 'brew install ffmpeg', watches_exit: true };
    expect(parseApproved(payload({ by: 'phone', install }))?.install).toEqual(install);
  });

  it('drops a payload missing what the sheet needs, and reads an odd one safely', () => {
    expect(parseApproved(null)).toBeNull();
    expect(parseApproved('x')).toBeNull();
    expect(parseApproved(payload({ provider_id: '' }))).toBeNull();
    expect(parseApproved(payload({ workspace_id: undefined }))).toBeNull();
    // An unknown `by` is not a phone, and an install without a tab is none.
    const odd = parseApproved(payload({ by: 'watch', install: { command_shown: 'x' } }));
    expect(odd?.by).toBe('desktop');
    expect(odd?.install).toBeNull();
  });
});

describe('the opening queue', () => {
  const o = (ws: string, at: number, over: Partial<ToolSetupOpening> = {}): ToolSetupOpening => ({ ...openingFrom(parseApproved(payload({ workspace_id: ws, request_id: `r_${ws}_${at}` }))!, at), ...over });

  it('holds one opening per Hive, the newest, and hands it over once', () => {
    const q = createOpeningQueue();
    q.add(o('a', 2));
    q.add(o('a', 1)); // older: ignored
    q.add(o('b', 3));
    expect(q.size).toBe(2);
    expect(q.next()?.workspaceId).toBe('b');
    expect(q.peek('a')?.at).toBe(2);
    expect(q.size).toBe(2);
    expect(q.take('a')?.at).toBe(2);
    expect(q.take('a')).toBeNull();
    expect(q.next()?.workspaceId).toBe('b');
  });

  it('keeps the end of a phone-started install that came before its sheet opened', () => {
    const q = createOpeningQueue();
    q.add(o('a', 1, { install: { tab_id: 't1', command_shown: 'brew install sox', watches_exit: true } }));
    const end = { provider_id: 'homebrew-kit', tab_id: 't1', ok: true, exit_code: 0, ended: 'exited', model_download: false };
    expect(q.installEnded({ ...end, tab_id: 'other' })).toBe(false);
    expect(q.installEnded(end)).toBe(true);
    expect(q.take('a')?.finished).toEqual(end);
  });
});

describe('one try per opening', () => {
  const o = (ws: string, id: string, at: number, over: Partial<ToolSetupOpening> = {}): ToolSetupOpening => ({ ...openingFrom(parseApproved(payload({ workspace_id: ws, request_id: id }))!, at), ...over });
  const done = (id: string, capability: string) => req({ id, decided_by: 'phone', args: { hive_id: 'h', workspace_id: 'ws', capability, provider_id: 'elevenlabs', hosts: [] } });

  it('a replaced opening is spent, so a reload never revives it', () => {
    // From the phone: voice approved, then music, for one Hive; focus opens music.
    const q = createOpeningQueue();
    q.add(o('ws', 'voice', 1));
    q.add(o('ws', 'music', 2));
    expect(q.take('ws')?.requestId).toBe('music');
    expect([...q.spent].sort()).toEqual(['music', 'voice']);
    // Cmd+R ten minutes later: recovery finds neither.
    expect(missedOpenings([done('voice', 'voice'), done('music', 'music')], 'main', new Set(['ws']), q.spent, 2_000)).toEqual([]);
  });

  it('an older one arriving after a newer is spent at once, and a spent one is never queued again', () => {
    const q = createOpeningQueue(new Set(['old']));
    q.add(o('ws', 'old', 5));
    expect(q.size).toBe(0);
    q.add(o('ws', 'new', 3));
    q.add(o('ws', 'older', 1));
    expect(q.peek('ws')?.requestId).toBe('new');
    expect(q.spent.has('older')).toBe(true);
    // The same request again (the event and recovery both) stays one opening, unspent.
    q.add(o('ws', 'new', 3));
    expect(q.size).toBe(1);
    expect(q.spent.has('new')).toBe(false);
  });

  it('a dropped opening and an ignored event are spent too', () => {
    const q = createOpeningQueue();
    q.add(o('ws', 'gone', 1));
    q.drop('ws');
    q.spend('suspended');
    expect(q.size).toBe(0);
    expect(missedOpenings([done('gone', 'voice'), done('suspended', 'music')], 'main', new Set(['ws']), q.spent, 2_000)).toEqual([]);
  });

  it('keeps the end of a taken opening’s install for the sheet that adopts it, a while', () => {
    const q = createOpeningQueue();
    q.add(o('ws', 'kit', 1, { install: { tab_id: 't1', command_shown: 'brew install sox', watches_exit: true } }));
    expect(q.take('ws', 1_000)).not.toBeNull();
    expect(q.endOf('t1', 1_000)).toBeNull();
    // The end comes after the take, before the sheet listens.
    const end = { provider_id: 'homebrew-kit', tab_id: 't1', ok: false, exit_code: 1, ended: 'exited', model_download: false };
    expect(q.installEnded(end)).toBe(true);
    expect(q.endOf('t1', 2_000)).toEqual(end);
    expect(q.endOf('t1', 1_000 + 10 * 60 * 1000 + 1)).toBeNull();
    expect(q.endOf('other', 2_000)).toBeNull();
  });
});

describe('missedOpenings', () => {
  const ws = new Set(['ws']);
  const now = 2_000;

  it('recovers an approval this window filed, in one of its Hives, not yet opened', () => {
    const got = missedOpenings([req({})], 'main', ws, new Set(), now);
    expect(got).toEqual([{ requestId: 'r_1', workspaceId: 'ws', capability: 'voice', providerId: 'elevenlabs', install: null, finished: null, at: 1_500 }]);
  });

  it('leaves what isn’t its own, isn’t done, was opened, or is too old', () => {
    expect(missedOpenings([req({ window_label: 'other' })], 'main', ws, new Set(), now)).toEqual([]);
    expect(missedOpenings([req({ args: { ...req({}).args, workspace_id: 'elsewhere' } })], 'main', ws, new Set(), now)).toEqual([]);
    expect(missedOpenings([req({ kind: 'hive_settings' })], 'main', ws, new Set(), now)).toEqual([]);
    for (const status of ['pending', 'approved', 'failed', 'denied', 'expired'] as const) {
      expect(missedOpenings([req({ status })], 'main', ws, new Set(), now)).toEqual([]);
    }
    expect(missedOpenings([req({})], 'main', ws, new Set(['r_1']), now)).toEqual([]);
    expect(missedOpenings([req({})], 'main', ws, new Set(), 1_500 + 60 * 60 * 1000 + 1)).toEqual([]);
    // Settled done but not an approval Rust carried out (a gone Hive settles failed, but be safe).
    expect(missedOpenings([req({ outcome: 'its Hive is gone' })], 'main', ws, new Set(), now)).toEqual([]);
  });

  it('never recovers one whose install already runs: a sheet without its tab would offer a second', () => {
    const installing = req({ outcome: 'Approved: Homebrew kit is installing in a tab on the Mac (`brew install ffmpeg`). finish joins the Hive once it works.' });
    expect(missedOpenings([installing], 'main', ws, new Set(), now)).toEqual([]);
    // A desktop approval is recovered too (the window reloaded at the click).
    const desktop = req({ decided_by: 'desktop', outcome: 'Approved: ElevenLabs’s setup sheet is open in the Hive’s window. voice joins the Hive once it works.' });
    expect(missedOpenings([desktop], 'main', ws, new Set(), now)).toHaveLength(1);
  });
});

describe('seededSetup', () => {
  it('opens the row’s recommended provider, with the row’s label and the baseline of what is ready', () => {
    const rows = [
      row('voice', { recommended: 'elevenlabs', alternatives: ['minimax'] }),
      row('finish', { covered_by: [{ tool_id: 'cli:ffmpeg', provider: 'homebrew-kit', state: 'ready' } as HiveMediaRow['covered_by'][number]] }),
    ];
    const got = seededSetup(rows, catalogue, { capability: 'voice', providerId: 'elevenlabs' });
    expect('error' in got).toBe(false);
    if ('error' in got) return;
    expect(got.provider.id).toBe('elevenlabs');
    expect(got.provider.creditsLine).toBe("Spends your ElevenLabs plan's credits");
    expect(got.label).toBe('Voice');
    expect(got.readyBefore).toEqual(['cli:ffmpeg']);
  });

  it('opens an alternative the card named, and a provider the row no longer offers', () => {
    const rows = [row('voice', { recommended: 'elevenlabs', alternatives: ['minimax'] }), row('music')];
    const alt = seededSetup(rows, catalogue, { capability: 'voice', providerId: 'minimax' });
    expect('error' in alt ? null : alt.provider.name).toBe('MiniMax');
    // Music became covered meanwhile, so the row recommends nothing: the approved provider still opens.
    const gone = seededSetup(rows, catalogue, { capability: 'music', providerId: 'elevenlabs' });
    expect('error' in gone ? null : [gone.provider.name, gone.label]).toEqual(['ElevenLabs', 'Music']);
  });

  it('says so for a provider this build’s catalogue doesn’t know, never guessing one', () => {
    const got = seededSetup([row('voice')], catalogue, { capability: 'voice', providerId: 'nope' });
    expect(got).toEqual({ error: 'The approved setup names a provider this maiTerm doesn’t know (nope).' });
  });
});
