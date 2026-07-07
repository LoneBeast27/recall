// Module registry — the design-for-more seam. A future project docks into the
// hub by adding one entry here plus a route directory.
export interface HubModule {
  id: string;
  title: string;
  icon: string; // inline emoji/glyph for v1; real icons with the design pass
  route: string;
}

export const MODULES: HubModule[] = [
  { id: 'clips', title: 'Clips', icon: '🎬', route: '/' },
  { id: 'hdr', title: 'HDR', icon: '🌗', route: '/hdr' },
  { id: 'audio', title: 'Audio', icon: '🎚️', route: '/audio' },
];
