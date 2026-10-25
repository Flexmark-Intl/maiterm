/** The gallery's pure helpers (docs/hive.md §10). Stored files reach the webview only through
 *  Rust's `gallery://` scheme, by asset id (hive/gallery/serve.rs): never a filesystem path. */

import { isWindows } from '$lib/utils/platform';

/** What `/<hive>/<asset>[/part]` serves: the stored file, its poster, a video's hover strip
 *  (ten frames across), an audio file's waveform (white on transparent, for tinting). */
export type GalleryPart = 'file' | 'thumb' | 'strip' | 'wave';

/** An asset's URL. Windows serves a custom scheme as `http://<scheme>.localhost`. */
export function galleryUrl(hiveId: string, assetId: string, part: GalleryPart = 'file', windows = isWindows()): string {
  const base = windows ? 'http://gallery.localhost' : 'gallery://localhost';
  const tail = part === 'file' ? '' : `/${part}`;
  return `${base}/${encodeURIComponent(hiveId)}/${encodeURIComponent(assetId)}${tail}`;
}
