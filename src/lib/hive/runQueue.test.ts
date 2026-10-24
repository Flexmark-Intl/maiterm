import { describe, expect, it } from 'vitest';
import { fromRows, prefixError, resourcesText, toRows } from './runQueue';
import type { RunQueueSettings } from '$lib/tauri/types';

const rq: RunQueueSettings = {
  commands: [
    {
      folder: 'app',
      prefix: 'npm run tauri:dev:isolated',
      needs: ['tauri-dev', 'cargo'],
      services: ['web'],
      env: [['MAITERM_INSTANCE', '{worker}'], ['URL', 'http://x/?a=b']],
      extra_write: ['~/Library/Caches/com.aiterm.dev.{worker}'],
    },
  ],
  resources: { cargo: 1 },
};

describe('run queue form', () => {
  it('round-trips the settings through its text', () => {
    expect(fromRows(toRows(rq), resourcesText(rq))).toEqual(rq);
  });

  it('tidies what the human typed', () => {
    const [row] = toRows(rq);
    const out = fromRows([{ ...row, prefix: '  cargo   test ', needs: 'cargo, , gpu', env: '# a comment\nA=1\nnot-a-pair\n', extraWrite: '\n/a\n\n' }], 'cargo=x\ngpu = 1');
    expect(out.commands[0].prefix).toBe('cargo test');
    expect(out.commands[0].needs).toEqual(['cargo', 'gpu']);
    expect(out.commands[0].env).toEqual([['A', '1']]);
    expect(out.commands[0].extra_write).toEqual(['/a']);
    expect(out.resources).toEqual({ cargo: 0, gpu: 1 });
  });

  it('says early what hiveRun would refuse in a prefix', () => {
    expect(prefixError('cargo test')).toBeNull();
    expect(prefixError('npx playwright test --grep=@smoke')).toBeNull();
    for (const bad of ['cargo test; id', 'a | b', 'a && b', 'echo $HOME', 'a `b`', 'a > f', '(a)', 'a \\ b', "a 'b'", 'a "b"', 'a\nb']) {
      expect(prefixError(bad), bad).not.toBeNull();
    }
    expect(prefixError('  ')).toMatch(/leading words/);
  });
});
