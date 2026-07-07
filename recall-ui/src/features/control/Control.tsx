import { useStore } from "../../store";
import { StatusCard } from "./StatusCard";

const MODES: { id: "auto" | "sdr" | "hdr"; label: string }[] = [
  { id: "auto", label: "Auto" },
  { id: "sdr", label: "SDR" },
  { id: "hdr", label: "HDR" },
];

export function Control() {
  const {
    running,
    monitors,
    selectedMonitor,
    status,
    error,
    log,
    start,
    stop,
    setMode,
    setSelectedMonitor,
  } = useStore();

  const activeSelect = status?.select ?? "auto";

  return (
    <div className="control">
      {error && <div className="error-banner">⚠ {error}</div>}
      <section className="card">
        <h2 className="card-title">Source</h2>
        <div className="field">
          <label>Monitor</label>
          <select
            value={selectedMonitor ?? ""}
            disabled={running || monitors.length === 0}
            onChange={(e) => setSelectedMonitor(Number(e.target.value))}
          >
            {monitors.length === 0 && <option value="">Start to detect…</option>}
            {monitors.map((m) => {
              const clean = m.name.replace(/[^A-Za-z0-9 ]/g, "");
              const label = clean.length > 0 ? clean : `Display ${m.index + 1}`;
              return (
                <option key={m.index} value={m.index}>
                  {label} · {m.width}×{m.height}
                  {m.primary ? " (primary)" : ""}
                </option>
              );
            })}
          </select>
        </div>
        <div className="actions">
          {running ? (
            <button className="btn danger" onClick={stop}>
              Stop
            </button>
          ) : (
            <button className="btn primary" onClick={start}>
              Start
            </button>
          )}
        </div>
      </section>

      <StatusCard running={running} status={status} />

      <section className="card">
        <h2 className="card-title">Tonemap mode</h2>
        <div className="segmented">
          {MODES.map((m) => (
            <button
              key={m.id}
              className={`seg${activeSelect === m.id ? " active" : ""}`}
              disabled={!running}
              onClick={() => setMode(m.id)}
            >
              {m.label}
            </button>
          ))}
        </div>
        <p className="hint">
          Auto follows the display's HDR state. SDR/HDR force passthrough/tonemap.
        </p>
      </section>

      <section className="card">
        <h2 className="card-title">Engine log</h2>
        <div className="log">
          {log.length === 0 ? (
            <span className="log-empty">No events yet.</span>
          ) : (
            log.map((line, i) => (
              <div
                key={i}
                className={`log-line${line.includes("[error]") || line.includes("[stderr]") ? " err" : ""}`}
              >
                {line}
              </div>
            ))
          )}
        </div>
      </section>
    </div>
  );
}
