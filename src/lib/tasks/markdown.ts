/** Longer agent text about a task (a blocker's context) as markdown, with any task the agent named
 *  by its id shown by its title, as `TaskText` does for one-liners. Rendered by the hardened
 *  transcript renderer: this is agent text going into the app's own webview. */

import { escapeHtml, renderTurnMarkdown } from '$lib/loom/markdown';
import { taskTextParts } from '$lib/tasks/model';
import type { Task } from '$lib/tauri/types';

/** Private-use characters, so a task reference survives the parse untouched. */
const OPEN = '\uE000';
const CLOSE = '\uE001';
const SLOT = new RegExp(`${OPEN}(\\d+)${CLOSE}`, 'g');

export function renderTaskMarkdown(text: string, tasks: Pick<Task, 'id' | 'title' | 'status'>[]): string {
  const clean = text.replace(new RegExp(`[${OPEN}${CLOSE}]`, 'g'), '');
  const refs: { title: string; token: string }[] = [];
  const marked = taskTextParts(clean, tasks)
    .map((p) => {
      if (!('task' in p)) return p.text;
      refs.push({ title: p.task.title, token: p.token });
      return `${OPEN}${refs.length - 1}${CLOSE}`;
    })
    .join('');
  // A reference inside a tag is part of an attribute (a link's address): it goes back to what
  // the agent wrote, since a span there would break the tag. The renderer escapes `>` in
  // attribute values, so a tag ends at its first one.
  return renderTurnMarkdown(marked).replace(new RegExp(`<[^>]*>|${SLOT.source}`, 'g'), (m, slot?: string) =>
    slot === undefined
      ? m.replace(SLOT, (_, i) => escapeHtml(refs[Number(i)].token))
      : `<span class="ref">“${escapeHtml(refs[Number(slot)].title)}”</span>`,
  );
}
