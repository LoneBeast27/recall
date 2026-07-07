import { useEffect, useState } from "react";
import { Titlebar } from "./components/Titlebar";
import { Nav, type NavId } from "./components/Nav";
import { Control } from "./features/control/Control";
import { Stub } from "./features/Stub";
import { useStore } from "./store";
import "./App.css";

export default function App() {
  const init = useStore((s) => s.init);
  const [active, setActive] = useState<NavId>("control");

  useEffect(() => {
    init();
  }, [init]);

  return (
    <div className="app">
      <Titlebar />
      <div className="body">
        <Nav active={active} onSelect={setActive} />
        <main className="pane">
          {active === "control" ? <Control /> : <Stub id={active} />}
        </main>
      </div>
    </div>
  );
}
