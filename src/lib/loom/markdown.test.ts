import { describe, expect, it } from 'vitest';
import { renderTurnMarkdown } from './markdown';

describe('transcript markdown is inert', () => {
  it('shows raw HTML as text', () => {
    const out = renderTurnMarkdown('hi <img src=x onerror="alert(1)"> <script>alert(2)</script>');
    expect(out).not.toMatch(/<img|<script/i);
    expect(out).toContain('&lt;img');
  });

  it('keeps http links and drops script links', () => {
    expect(renderTurnMarkdown('[ok](https://maiterm.dev)')).toContain('href="https://maiterm.dev"');
    const bad = renderTurnMarkdown('[x](javascript:alert(1))');
    expect(bad).not.toMatch(/href=/);
    expect(bad).toContain('x');
  });

  it('never loads an image', () => {
    const out = renderTurnMarkdown('![diagram](https://evil.example/track.png)');
    expect(out).not.toMatch(/<img/);
    expect(out).toContain('diagram');
  });

  it('still renders the markdown agents actually write', () => {
    const out = renderTurnMarkdown('**bold** and `code`\n\n- a\n- b');
    expect(out).toContain('<strong>bold</strong>');
    expect(out).toContain('<code>code</code>');
    expect(out).toContain('<li>a</li>');
  });
});
