/** File attachments for a composer: what a paste or a drop becomes before it is sent. Shared by
 *  the terminal's composer dock and the Loom's Focus composer, which send differently (one types
 *  into the PTY, the other goes through `send_tab_message`) but capture the same way. */

import { readText as clipboardReadText, readImage as clipboardReadImage } from '@tauri-apps/plugin-clipboard-manager';
import { readClipboardFilePaths, saveClipboardImage } from '$lib/tauri/commands';
import { encodeClipboardImage } from '$lib/utils/clipboardImage';

export interface ComposerAttachment {
  /** Local absolute path (pasted screenshots are materialized to a temp file). */
  path: string;
  name: string;
  /** Small data: URL preview — only for pasted screenshots, where the pixels are already in
   *  hand. Dropped/copied files get a generic icon. */
  thumb?: string;
}

export function basename(p: string): string {
  return p.split('/').pop() ?? p;
}

export function fromPaths(paths: string[]): ComposerAttachment[] {
  return paths.map((p) => ({ path: p, name: basename(p) }));
}

/** `next` added to `current`, deduped by path: re-pasting the same Finder selection shouldn't
 *  stack chips. */
export function mergeAttachments(current: ComposerAttachment[], next: ComposerAttachment[]): ComposerAttachment[] {
  const existing = new Set(current.map((a) => a.path));
  return [...current, ...next.filter((a) => !existing.has(a.path))];
}

/** Downscaled data: URL for the chip preview (≤48px tall, 2x for retina). */
async function makeThumb(rgba: Uint8Array, width: number, height: number): Promise<string> {
  const src = new OffscreenCanvas(width, height);
  src.getContext('2d')!.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
  const scale = Math.min(1, 48 / height);
  const dst = new OffscreenCanvas(Math.max(1, Math.round(width * scale)), Math.max(1, Math.round(height * scale)));
  dst.getContext('2d')!.drawImage(src, 0, 0, dst.width, dst.height);
  const blob = await dst.convertToBlob({ type: 'image/png' });
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return `data:image/png;base64,${btoa(binary)}`;
}

export type PasteResult =
  | { kind: 'attachments'; items: ComposerAttachment[] }
  | { kind: 'text'; text: string }
  | null;

/** Read the native pasteboard the way the terminal's paste does: Finder file copies become
 *  attachments; screenshot image data becomes a temp PNG attachment with a preview; otherwise
 *  plain text. WKWebView's clipboardData misses the first two, hence the native reads. */
export async function readPaste(): Promise<PasteResult> {
  const paths = await readClipboardFilePaths();
  if (paths.length > 0) return { kind: 'attachments', items: fromPaths(paths) };
  try {
    const image = await clipboardReadImage();
    const { width, height } = await image.size();
    if (width > 0 && height > 0) {
      const rgba = await image.rgba();
      const { base64, ext } = await encodeClipboardImage(rgba, width, height);
      const localPath = await saveClipboardImage(base64, ext);
      const thumb = await makeThumb(rgba, width, height);
      return { kind: 'attachments', items: [{ path: localPath, name: basename(localPath), thumb }] };
    }
  } catch {
    // No image on the clipboard — fall through to text.
  }
  const text = await clipboardReadText();
  return text ? { kind: 'text', text } : null;
}

/** A menu or context paste carrying files or an image, which the default textarea paste would
 *  drop. Plain text keeps the default paste (it preserves the undo stack). */
export function pasteCarriesFiles(e: ClipboardEvent): boolean {
  const cd = e.clipboardData;
  return !!cd && (cd.files.length > 0 || [...cd.items].some((i) => i.kind === 'file'));
}
