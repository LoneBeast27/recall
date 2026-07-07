import type { Status } from "../../store";

export function StatusCard({
  running,
  status,
}: {
  running: boolean;
  status: Status | null;
}) {
  if (!running) {
    return (
      <section className="card status idle">
        <span className="status-dot off" />
        <span className="status-idle-text">Engine stopped</span>
      </section>
    );
  }

  const isHdr = status?.display === "hdr";

  return (
    <section className="card status">
      <div className="status-head">
        <span className={`status-dot ${isHdr ? "hdr" : "sdr"}`} />
        <span className="status-live">Live</span>
        <span className="status-source">{status?.source ?? "—"} SDR</span>
      </div>
      <div className="status-grid">
        <Metric label="Display" value={status?.display?.toUpperCase() ?? "—"} accent={isHdr} />
        <Metric label="Mode" value={status?.mode ?? "—"} />
        <Metric
          label="Resolution"
          value={status && status.width > 0 ? `${status.width}×${status.height}` : "—"}
        />
        <Metric label="FPS" value={status ? String(status.fps) : "—"} />
        <Metric label="Peak" value={status ? `${Math.round(status.max_nits)} nits` : "—"} />
      </div>
    </section>
  );
}

function Metric({
  label,
  value,
  accent,
}: {
  label: string;
  value: string;
  accent?: boolean;
}) {
  return (
    <div className="metric">
      <span className="metric-label">{label}</span>
      <span className={`metric-value${accent ? " accent" : ""}`}>{value}</span>
    </div>
  );
}
