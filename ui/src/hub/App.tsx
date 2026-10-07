import { useEffect, useState } from "preact/hooks";
import { daemon, onNavigate } from "../lib/daemon";
import { HubContext, useHub, useHubState } from "./useDaemon";
import { Home } from "./pages/Home";
import { History } from "./pages/History";
import { Dictionary } from "./pages/Dictionary";
import { Settings } from "./pages/Settings";
import { Doctor } from "./pages/Doctor";

const PAGES = [
  { id: "home", title: "Home", render: () => <Home /> },
  { id: "history", title: "History", render: () => <History /> },
  { id: "dictionary", title: "Dictionary", render: () => <Dictionary /> },
  { id: "settings", title: "Settings", render: () => <Settings /> },
  { id: "doctor", title: "Doctor", render: () => <Doctor /> },
] as const;
type PageId = (typeof PAGES)[number]["id"];

function pageFromHash(): PageId {
  const id = window.location.hash.replace(/^#\/?/, "");
  return (PAGES.find((p) => p.id === id)?.id ?? "home") as PageId;
}

export function App() {
  const hub = useHubState();
  const [page, setPage] = useState<PageId>(pageFromHash());

  useEffect(() => {
    const onHash = () => setPage(pageFromHash());
    window.addEventListener("hashchange", onHash);
    const off = onNavigate((p) => {
      window.location.hash = `#/${p}`;
    });
    return () => {
      window.removeEventListener("hashchange", onHash);
      void off.then((f) => f());
    };
  }, []);

  const current = PAGES.find((p) => p.id === page) ?? PAGES[0];
  const connected = hub.connection.status === "connected";

  return (
    <HubContext.Provider value={hub}>
      <div class="shell">
        <nav class="sidebar" aria-label="Sections">
          <div class="brand">
            <span class={`status-dot ${connected ? "on" : "off"}`} aria-hidden="true" />
            dictate
          </div>
          <ul>
            {PAGES.map((p) => (
              <li key={p.id}>
                <a href={`#/${p.id}`} aria-current={p.id === page ? "page" : undefined}>
                  {p.title}
                </a>
              </li>
            ))}
          </ul>
          <ConnectionFooter />
        </nav>
        <main class="content" id="main" tabIndex={-1}>
          {!connected && <DisconnectedBanner />}
          {current.render()}
        </main>
      </div>
    </HubContext.Provider>
  );
}

function ConnectionFooter() {
  const { connection } = useHub();
  if (connection.status !== "connected") return <p class="footer muted">daemon offline</p>;
  return (
    <p class="footer muted">
      {connection.server} {connection.version}
    </p>
  );
}

function DisconnectedBanner() {
  const { connection } = useHub();
  if (connection.status === "connected") return null;
  return (
    <section class="banner warn" role="alert">
      <div>
        <strong>dictated is not running.</strong>{" "}
        {connection.status === "disconnected" ? connection.reason : "Connecting…"}
        <div class="muted small">
          Start it with <code>systemctl --user start dictated</code> (or run <code>dictated</code>). This window reconnects
          on its own.
        </div>
      </div>
      <button type="button" onClick={() => void daemon.reconnect()}>
        Retry now
      </button>
    </section>
  );
}
