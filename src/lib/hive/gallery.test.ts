import { describe, expect, it } from 'vitest';
import { galleryUrl } from './gallery';

describe('galleryUrl', () => {
  const hive = '6f1c2e7a-3b0d-4c55-9a1e-2f7d8c9b0a11';
  it('uses the custom scheme on macOS and Linux', () => {
    expect(galleryUrl(hive, 'a-0123456789ab', 'file', false)).toBe(`gallery://localhost/${hive}/a-0123456789ab`);
    expect(galleryUrl(hive, 'a-0123456789ab', 'thumb', false)).toBe(`gallery://localhost/${hive}/a-0123456789ab/thumb`);
  });

  it("uses Tauri's http form on Windows", () => {
    expect(galleryUrl(hive, 'a-0123456789ab', 'strip', true)).toBe(`http://gallery.localhost/${hive}/a-0123456789ab/strip`);
    expect(galleryUrl(hive, 'a-0123456789ab', undefined, true)).toBe(`http://gallery.localhost/${hive}/a-0123456789ab`);
  });

  it('escapes, so an id can never add a component', () => {
    expect(galleryUrl('../x', 'a/b', 'wave', false)).toBe('gallery://localhost/..%2Fx/a%2Fb/wave');
  });
});
