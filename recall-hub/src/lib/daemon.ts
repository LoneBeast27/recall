// Daemon feed: every control-pipe line arrives as a `daemon` Tauri event.
// Stores hold the latest state per module; commands go back via daemon_send.
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';
import type { EditPlan } from '$lib/editor';

export interface EngineStatus {
  display: string;
  max_nits: number;
  mode: string;
  select: string;
  source: string;
  width: number;
  height: number;
  fps: number;
  buffer_secs: number;
  encoder: string;
  /** Native capture timestamps are retained in saved recordings. */
  recording_mode?: 'vfr' | string;
  /** Present when the NVENC encoder could not initialize or continue. */
  encoder_error?: string | null;
}

export interface HdrStatus {
  target: string;
  hdr: boolean;
  manual_pin: boolean;
  session_pid: number | null;
  auto: boolean;
  engine_health: string;
}

export interface SavedEvent {
  path: string;
  seconds: number;
  frames: number;
  bytes: number;
  elapsed_ms: number;
  at: number;
}

export interface ExportStartedEvent {
  path: string;
  source: string;
  fps: number;
  at: number;
}

export interface ExportedEvent {
  path: string;
  source: string;
  fps: number;
  frames: number;
  seconds: number;
  elapsed_ms: number;
  at: number;
}

export const connected = writable(false);
export const engine = writable<EngineStatus | null>(null);
export const hdr = writable<HdrStatus | null>(null);
export const lastSaved = writable<SavedEvent | null>(null);
export const saveError = writable<string | null>(null);
export const exportStarted = writable<ExportStartedEvent | null>(null);
export const lastExported = writable<ExportedEvent | null>(null);
export const exportError = writable<string | null>(null);

let started = false;
let localExport = false;

/** Wire the event stream once (idempotent; called from the layout). */
export async function startDaemonFeed() {
  if (started) return;
  started = true;
  // The backend connects (and receives the one-shot snapshots) before this
  // page exists — request a fresh HDR snapshot now that we're listening.
  const requestSnapshot = () =>
    invoke('daemon_send', { line: '{"cmd":"status"}' }).catch(() => {});
  setTimeout(requestSnapshot, 300);
  await listen<string>('daemon', (e) => {
    let msg: { mod?: string; event?: Record<string, unknown> };
    try {
      msg = JSON.parse(e.payload);
    } catch {
      return;
    }
    const ev = (msg.event ?? {}) as Record<string, unknown>;
    const type = ev.type as string | undefined;
    // Any daemon traffic proves the pipe is up (a late-joining page misses the
    // one-shot connected event; engine status flows ~1/s regardless).
    if (msg.mod !== 'hub' || type !== 'disconnected') connected.set(true);
    switch (msg.mod) {
      case 'hub':
        if (type === 'disconnected') {
          connected.set(false);
          if (get(exportStarted) && !localExport) {
            exportStarted.set(null);
            exportError.set('Connection lost while exporting. Check the output folder before retrying.');
          }
        }
        if (type === 'connected') requestSnapshot();
        break;
      case 'engine':
        if (type === 'status') engine.set(ev as unknown as EngineStatus);
        if (type === 'saved') {
          lastSaved.set({ ...(ev as unknown as SavedEvent), at: Date.now() });
          saveError.set(null);
        }
        if (type === 'save_failed') saveError.set(String(ev.reason ?? 'unknown'));
        if (type === 'export_started') {
          exportStarted.set({ ...(ev as unknown as Omit<ExportStartedEvent, 'at'>), at: Date.now() });
          exportError.set(null);
        }
        if (type === 'exported') {
          lastExported.set({ ...(ev as unknown as Omit<ExportedEvent, 'at'>), at: Date.now() });
          exportStarted.set(null);
          exportError.set(null);
        }
        if (type === 'export_failed') {
          exportStarted.set(null);
          exportError.set(String(ev.reason ?? 'unknown'));
        }
        break;
      case 'hdr':
        if (type === 'status') hdr.set(ev as unknown as HdrStatus);
        break;
    }
  });
}

const send = (cmd: Record<string, unknown>) =>
  invoke('daemon_send', { line: JSON.stringify(cmd) });

export const saveClip = (seconds?: number) =>
  send(seconds ? { cmd: 'save_clip', seconds } : { cmd: 'save_clip' });
export const exportClip = async (path: string, fps = 60, edit?: EditPlan) => {
  if (!edit) return send({ cmd: 'export_clip', path, fps });
  localExport = true;
  try {
    const result = await invoke<Omit<ExportedEvent, 'at'>>('export_editor', { path, fps, plan: edit });
    lastExported.set({ ...result, at: Date.now() });
    exportError.set(null);
  } finally {
    localExport = false;
    exportStarted.set(null);
  }
};
export const toggleHdr = () => send({ cmd: 'toggle_hdr' });
export const setMode = (value: 'auto' | 'sdr' | 'hdr') => send({ cmd: 'set_mode', value });
export const setAcProfile = (variant: 'punchy' | 'accurate') =>
  send({ cmd: 'set_ac_profile', variant });

export interface ClipEntry {
  name: string;
  path: string;
  mtime: number;
  bytes: number;
  thumb: string | null;
}
export const listClips = (limit = 8) => invoke<ClipEntry[]>('list_clips', { limit });
export const showInFolder = (path: string) => invoke('show_in_folder', { path });
