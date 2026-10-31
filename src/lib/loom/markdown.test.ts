import { describe, expect, it } from 'vitest';
import { renderTurnMarkdown } from './markdown';

describe('transcript markdown is inert', () => {
  it('shows raw HTML as text', () => {
    const out = renderTurnMarkdown('hi <img src=x onerror="alert(1)"> <script>alert(2)</script>');
    expect(out).not.toMatch(/<img|<script/i);
    expect(out).toContain('&lt;img');
  });

  // marked stops escaping plain text after an inline <pre>, <code>, <kbd> or <script>, and an
  // unclosed `<x` is plain text, not a tag: the paragraph's own </p> then closes it.
  it.each(['pre', 'code', 'kbd', 'script'])('escapes text that follows an inline <%s>', (tag) => {
    const out = renderTurnMarkdown(`Before <${tag}> <img src=x onerror=window.pwned=1 //`);
    expect(out).not.toMatch(/<img/i);
    expect(out).toContain('&lt;img');
  });

  it('gives no way to a tag that navigates or restyles the app', () => {
    const refresh = renderTurnMarkdown('Before <code> <meta http-equiv=refresh content="0;url=&#104;ttp://evil.example/" ');
    expect(refresh).not.toMatch(/<meta/i);
    const style = renderTurnMarkdown('Before <code> <style x\n\nx{}#h{display:none}');
    expect(style).not.toMatch(/<style/i);
  });

  it('keeps every word of prose that mentions a tag, across paragraphs', () => {
    const out = renderTurnMarkdown('Wrap the log in <pre> and check x<y holds.\n\nSecond paragraph with a <b and more.');
    expect(out).toContain('x&lt;y holds.');
    expect(out).toContain('a &lt;b and more.');
  });

  it('keeps an entity the text already wrote as one character', () => {
    expect(renderTurnMarkdown('fish &amp; chips')).toContain('fish &amp; chips');
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

  it('turns an image into an inert placeholder carrying its escaped src', () => {
    const out = renderTurnMarkdown('![shot](/Users/me/a.png "t") ![x](/a/"><img src=y>.png)');
    expect(out).toContain('<span class="md-img" data-src="/Users/me/a.png">shot</span>');
    expect(out).not.toMatch(/<img/);
  });

  it('still renders the markdown agents actually write', () => {
    const out = renderTurnMarkdown('**bold** and `code`\n\n- a\n- b');
    expect(out).toContain('<strong>bold</strong>');
    expect(out).toContain('<code>code</code>');
    expect(out).toContain('<li>a</li>');
  });
});
