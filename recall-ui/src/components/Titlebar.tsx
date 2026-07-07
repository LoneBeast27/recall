import { getCurrentWindow } from "@tauri-apps/api/window";

// Called inside handlers (not at module load) so a plain-browser preview — where
// the Tauri runtime is absent — doesn't crash the whole app.
function win() {
  return getCurrentWindow();
}

export function Titlebar() {
  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="brand" data-tauri-drag-region>
        <span className="brand-mark" />
        <span className="brand-name">Recall</span>
      </div>
      <div className="win-controls">
        <button className="win-btn" onClick={() => win().minimize()} aria-label="Minimize">
          &#x2013;
        </button>
        <button
          className="win-btn"
          onClick={() => win().toggleMaximize()}
          aria-label="Maximize"
        >
          &#x25A1;
        </button>
        <button className="win-btn close" onClick={() => win().close()} aria-label="Close">
          &#x2715;
        </button>
      </div>
    </div>
  );
}
