#!/usr/bin/env node
// `npm run tauri:dev:isolated`: a dev maiTerm beside the dev and installed ones, touching
// neither (docs/hive.md §6.8, src-tauri/src/instance.rs). MAITERM_INSTANCE names it: its data
// folder, bundle identifier (so WebKit storage and logs) and ssh sockets are its own, and it
// makes no global writes. MAITERM_VITE_PORT is its dev server's port.
//
// Unlike `tauri:dev` it never kills whatever holds 1420 (that is the dev app's server), and it
// leaves out `--features mcp-bridge`, whose port is fixed and would collide with dev's.
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// The dev server's, the installed maiTerm's maiLink, and maiLink's old port (where a
// kill-by-port quit maiTerm). Mirrors `instance::RESERVED_PORTS`.
const RESERVED = new Set([1420, 8765, 27365]);

function fail(msg) {
  process.stderr.write(`tauri:dev:isolated: ${msg}\n`);
  process.exit(2);
}

function port(name, required) {
  const raw = process.env[name];
  if (raw === undefined || raw === '') {
    if (required) fail(`${name} is required (a free port from 1024 to 65535)`);
    return null;
  }
  const p = Number(raw);
  if (!/^\d+$/.test(raw) || p < 1024 || p > 65535) fail(`${name}=${raw} is not a port from 1024 to 65535`);
  if (RESERVED.has(p)) fail(`${name}=${p} is reserved: 1420, 8765 and 27365 belong to the other maiTerms`);
  return p;
}

const name = process.env.MAITERM_INSTANCE ?? '';
if (!/^[a-z0-9-]{1,32}$/.test(name)) fail(`MAITERM_INSTANCE=${JSON.stringify(name)} must be 1-32 of a-z, 0-9 and -`);
const vitePort = port('MAITERM_VITE_PORT', true);
const mcpPort = port('MAITERM_MCP_PORT', false);
const mailinkPort = port('MAITERM_MAILINK_PORT', false);
const taken = [mcpPort, mailinkPort].filter((p) => p !== null);
if (taken.includes(vitePort) || (mcpPort !== null && mcpPort === mailinkPort)) fail('the vite, MCP and maiLink ports must differ');

const identifier = `com.aiterm.dev.${name}`;
const devUrl = `http://localhost:${vitePort}`;
const override = JSON.stringify({
  identifier,
  build: { devUrl, beforeDevCommand: `npm run dev -- --port ${vitePort}` },
});

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tauri = path.join(root, 'node_modules', '.bin', process.platform === 'win32' ? 'tauri.cmd' : 'tauri');
const args = ['dev', '--config', 'src-tauri/tauri.dev.conf.json', '--config', override, ...process.argv.slice(2)];
process.stdout.write(`tauri:dev:isolated: ${identifier} on ${devUrl}${mcpPort ? `, MCP on ${mcpPort}` : ''}\n`);

const child = spawn(tauri, args, { cwd: root, stdio: 'inherit', env: process.env });
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => child.kill(sig));
child.on('exit', (code, signal) => process.exit(code ?? (signal ? 1 : 0)));
