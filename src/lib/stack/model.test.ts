import { describe, expect, it } from 'vitest';
import { detectEndpoint, exitIsCrash, launchableUrl, restartAllowed, restartDelay, rollupStatus, shq, startLine, RESTART_WINDOW_MS } from './model';

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
