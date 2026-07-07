import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Monitor = {
  index: number;
  name: string;
  width: number;
  height: number;
  primary: boolean;
};

export type Status = {
  display: "hdr" | "sdr";
  max_nits: number;
  mode: "tonemap" | "passthrough";
  select: "auto" | "sdr" | "hdr";
  source: string;
  width: number;
  height: number;
  fps: number;
};

type EngineMsg =
  | { type: "hello"; version: string; monitors: Monitor[] }
  | ({ type: "status" } & Status);

type Store = {
  running: boolean;
  version: string;
  monitors: Monitor[];
  selectedMonitor: number | null;
  status: Status | null;
  initialized: boolean;
  error: string | null;
  log: string[];

  init: () => Promise<void>;
  start: () => Promise<void>;
  stop: () => Promise<void>;
  setMode: (m: "auto" | "sdr" | "hdr") => Promise<void>;
  setSelectedMonitor: (i: number) => void;
};

function pushLog(set: (fn: (s: Store) => Partial<Store>) => void, line: string) {
  set((s) => ({ log: [...s.log.slice(-49), line] }));
}

export const useStore = create<Store>((set, get) => ({
  running: false,
  version: "",
  monitors: [],
  selectedMonitor: null,
  status: null,
  initialized: false,
  error: null,
  log: [],

  init: async () => {
    if (get().initialized) return;
    set({ initialized: true });

    // Tauri runtime absent (plain-browser preview) → skip event wiring.
    if (!("__TAURI_INTERNALS__" in window)) {
      pushLog(set, "[ui] Tauri runtime absent (browser preview)");
      return;
    }

    await listen<string>("engine:stdout", (e) => {
      let msg: EngineMsg;
      try {
        msg = JSON.parse(e.payload);
      } catch {
        pushLog(set, `[stdout] ${e.payload}`);
        return;
      }
      if (msg.type === "hello") {
        const primary = msg.monitors.find((m) => m.primary);
        pushLog(set, `[hello] v${msg.version}, ${msg.monitors.length} monitor(s)`);
        set((s) => ({
          version: msg.version,
          monitors: msg.monitors,
          selectedMonitor:
            s.selectedMonitor ?? primary?.index ?? msg.monitors[0]?.index ?? null,
        }));
      } else if (msg.type === "status") {
        const { type: _type, ...status } = msg;
        void _type;
        set({ status });
      }
    });

    await listen<string>("engine:stderr", (e) => {
      pushLog(set, `[stderr] ${e.payload}`);
    });

    await listen<number | null>("engine:terminated", (e) => {
      pushLog(set, `[engine] terminated (code ${e.payload ?? "?"})`);
      set({ running: false, status: null });
    });
  },

  start: async () => {
    const { selectedMonitor } = get();
    set({ error: null });
    pushLog(set, `[ui] start (monitor ${selectedMonitor ?? "primary"})`);
    try {
      await invoke("start_engine", { monitor: selectedMonitor ?? undefined });
      set({ running: true });
      pushLog(set, "[ui] engine spawned");
    } catch (e) {
      const msg = String(e);
      set({ error: msg, running: false });
      pushLog(set, `[error] start failed: ${msg}`);
    }
  },

  stop: async () => {
    try {
      await invoke("stop_engine");
    } catch (e) {
      pushLog(set, `[error] stop: ${String(e)}`);
    }
    set({ running: false, status: null });
    pushLog(set, "[ui] stopped");
  },

  setMode: async (m) => {
    try {
      await invoke("send_command", {
        json: JSON.stringify({ cmd: "set_mode", value: m }),
      });
      set((s) => (s.status ? { status: { ...s.status, select: m } } : {}));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  setSelectedMonitor: (i) => set({ selectedMonitor: i }),
}));
