/**
 * An editor tab's title when the file behind it changes: renamed or moved in Files, or swapped
 * by a preview or the agent's `openFile` with `targetTabId`.
 *
 * `custom_name` can't say whether the human chose the title: `Tab::new_editor` sets it on every
 * editor tab (the strip shows `tab.name` as is), the load migration sets it on any tab not named
 * "Terminal", and a human rename sets it too. What does tell them apart is the title itself: the
 * open paths title a tab with its file's name, so a title that is still that name follows the
 * file, and any other title is one the human typed and stays.
 */
import type { EditorFileInfo } from '$lib/tauri/types';
import { basenameOf } from './pathForm';

/** The title an editor tab gets for a file. A remote path is the ssh host's, always `/`. */
export function editorTitleFor(path: string, remote: boolean, windows: boolean): string {
  return remote ? path.split('/').pop() || path : basenameOf(path, windows);
}

/**
 * The title for a tab titled `title` whose file goes from `from` to `to`. The new file's name
 * when `title` is still the old file's name (or empty), else `title` unchanged. The old
 * agent-open path titled a Windows tab with `split('/')` (the whole path), which counts as the
 * file's name too.
 */
export function editorTitleAfter(
  title: string,
  from: Pick<EditorFileInfo, 'file_path' | 'is_remote'> | null | undefined,
  to: Pick<EditorFileInfo, 'file_path' | 'is_remote'>,
  windows: boolean,
): string {
  const next = editorTitleFor(to.file_path, to.is_remote, windows);
  if (!title || !from) return next;
  const auto = title === editorTitleFor(from.file_path, from.is_remote, windows)
    || title === (from.file_path.split('/').pop() || from.file_path);
  return auto ? next : title;
}
