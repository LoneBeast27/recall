export interface TimeRange { start: number; end: number }
export interface AudioSetting { stream_index: number; gain_db: number; muted: boolean }
export interface EditPlan { ranges: TimeRange[]; audio: AudioSetting[] }

export const EPSILON = 0.001;
export const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export function cleanRanges(ranges: TimeRange[], duration: number): TimeRange[] {
  const ordered = ranges
    .map((range) => ({ start: clamp(range.start, 0, duration), end: clamp(range.end, 0, duration) }))
    .filter((range) => range.end - range.start > EPSILON)
    .sort((a, b) => a.start - b.start);
  const result: TimeRange[] = [];
  for (const range of ordered) {
    const previous = result.at(-1);
    // Deliberately retain adjacent split segments: they are edit operations until removed or trimmed.
    if (previous && range.start < previous.end - EPSILON) previous.end = Math.max(previous.end, range.end);
    else result.push(range);
  }
  return result;
}

export function splitAt(ranges: TimeRange[], time: number, duration: number): TimeRange[] {
  for (let index = 0; index < ranges.length; index++) {
    const range = ranges[index];
    if (time > range.start + EPSILON && time < range.end - EPSILON) {
      return [...ranges.slice(0, index), { start: range.start, end: time }, { start: time, end: range.end }, ...ranges.slice(index + 1)];
    }
  }
  return ranges;
}

export function trimRange(ranges: TimeRange[], selected: number, edge: 'start' | 'end', time: number, duration: number): TimeRange[] {
  const range = ranges[selected];
  if (!range) return ranges;
  const next = ranges.map((item) => ({ ...item }));
  if (edge === 'start') next[selected].start = clamp(time, range.start, range.end - EPSILON);
  else next[selected].end = clamp(time, range.start + EPSILON, range.end);
  return cleanRanges(next, duration);
}

export function sourceToEdited(ranges: TimeRange[], sourceTime: number): number | null {
  let elapsed = 0;
  for (const range of ranges) {
    if (sourceTime >= range.start && sourceTime <= range.end) return elapsed + sourceTime - range.start;
    elapsed += range.end - range.start;
  }
  return null;
}

export function editedToSource(ranges: TimeRange[], editedTime: number): number {
  let elapsed = 0;
  for (const range of ranges) {
    const length = range.end - range.start;
    if (editedTime <= elapsed + length) return range.start + clamp(editedTime - elapsed, 0, length);
    elapsed += length;
  }
  return ranges.at(-1)?.end ?? 0;
}

export function nextRetainedTime(ranges: TimeRange[], sourceTime: number): number | null {
  for (const range of ranges) {
    if (sourceTime >= range.start - EPSILON && sourceTime < range.end - EPSILON) return sourceTime;
    if (sourceTime < range.start) return range.start;
  }
  return null;
}

export function makeHistory<T>(initial: T) {
  // Svelte 5 state is a Proxy and cannot be passed to structuredClone.
  const snapshot = (value: T): T => JSON.parse(JSON.stringify(value)) as T;
  let entries = [snapshot(initial)];
  let cursor = 0;
  return {
    push(value: T) { entries = [...entries.slice(0, cursor + 1), snapshot(value)]; cursor = entries.length - 1; return snapshot(value); },
    undo() { if (cursor > 0) cursor--; return snapshot(entries[cursor]); },
    redo() { if (cursor < entries.length - 1) cursor++; return snapshot(entries[cursor]); },
    get canUndo() { return cursor > 0; }, get canRedo() { return cursor < entries.length - 1; },
  };
}
