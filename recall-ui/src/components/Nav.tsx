export type NavId = "control" | "clipper" | "gallery" | "overlay";

const ITEMS: { id: NavId; label: string; enabled: boolean }[] = [
  { id: "control", label: "Control", enabled: true },
  { id: "clipper", label: "Clipper", enabled: false },
  { id: "gallery", label: "Gallery", enabled: false },
  { id: "overlay", label: "Overlay", enabled: false },
];

export function Nav({
  active,
  onSelect,
}: {
  active: NavId;
  onSelect: (id: NavId) => void;
}) {
  return (
    <nav className="nav">
      {ITEMS.map((item) => (
        <button
          key={item.id}
          className={`nav-item${active === item.id ? " active" : ""}`}
          disabled={!item.enabled}
          onClick={() => item.enabled && onSelect(item.id)}
        >
          <span>{item.label}</span>
          {!item.enabled && <span className="nav-soon">soon</span>}
        </button>
      ))}
    </nav>
  );
}
