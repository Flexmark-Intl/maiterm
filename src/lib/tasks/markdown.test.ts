import { describe, expect, it } from 'vitest';
import { renderTaskMarkdown } from './markdown';

const task = { id: 'd1ab499b-4f18-4379-8b12-1c7a1ae9238e', title: 'Ship <the> app', status: 'todo' as const };

describe('renderTaskMarkdown', () => {
  it('draws a numbered list, code, and a closing line outside the list', () => {
    const out = renderTaskMarkdown('1. One `__Host-x`\n2. Two\n\nNo worker starts.', []);
    expect(out).toContain('<ol>');
    expect(out.match(/<li>/g)).toHaveLength(2);
    expect(out).toContain('<code>__Host-x</code>');
    expect(out).toMatch(/<\/ol>\s*<p>No worker starts\.<\/p>/);
  });

  it('keeps a line break in plain prose', () => {
    expect(renderTaskMarkdown('first\nsecond', [])).toContain('<br>');
  });

  it('shows a task by its escaped title, never its id', () => {
    const out = renderTaskMarkdown('Waits on d1ab499b.', [task]);
    expect(out).toContain('<span class="ref">“Ship &lt;the&gt; app”</span>');
    expect(out).not.toContain('d1ab499b');
  });

  it('leaves a link address as written when it holds something shaped like a task id', () => {
    const out = renderTaskMarkdown('[run](https://ci.example/d1ab499b)', [task]);
    expect(out).toContain('href="https://ci.example/d1ab499b"');
    expect(out).not.toContain('class="ref"');
  });

  it('leaves digits alone beside a task reference', () => {
    const out = renderTaskMarkdown('Port 3140 waits on d1ab499b, 11 checks.', [task]);
    expect(out).toContain('Port 3140 waits on');
    expect(out).toContain('11 checks.');
  });

  it('shows raw HTML as text', () => {
    expect(renderTaskMarkdown('<img src=x onerror="alert(1)">', [task])).not.toContain('<img');
  });

  it('ignores slot characters the agent wrote itself', () => {
    expect(renderTaskMarkdown('a\uE0000\uE001b', [task])).toContain('a0b');
  });
});
