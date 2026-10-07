// Shared hub state: the connection, and a tick that bumps whenever a session
// finishes so pages showing history can refresh themselves.

import { createContext } from "preact";
import { useContext, useEffect, useState } from "preact/hooks";
import { daemon, onConnection, onDaemonEvent } from "../lib/daemon";
import type { Connection } from "../lib/protocol";

export interface HubState {
  connection: Connection;
  /** Increments when a session ends or the connection is (re)established. */
  refresh: number;
}

export const HubContext = createContext<HubState>({
  connection: { status: "connecting", attempt: 1 },
  refresh: 0,
});

export function useHub(): HubState {
  return useContext(HubContext);
}

export function useHubState(): HubState {
  const [connection, setConnection] = useState<Connection>({ status: "connecting", attempt: 1 });
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    void daemon.connectionState().then(setConnection).catch(() => {});
    const offs = [
      onConnection((c) => {
        setConnection(c);
        if (c.status === "connected") setRefresh((n) => n + 1);
      }),
      onDaemonEvent((e) => {
        if (e.type === "state_changed" && ["done", "error", "cancelled"].includes(String(e["to"]))) {
          setRefresh((n) => n + 1);
        }
      }),
    ];
    return () => offs.forEach((p) => void p.then((off) => off()));
  }, []);
  return { connection, refresh };
}

/** Load something from the daemon; reloads when `deps` change. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[]): {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
} {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    setLoading(true);
    load()
      .then((d) => {
        if (!live) return;
        setData(d);
        setError(null);
      })
      .catch((e: unknown) => {
        if (!live) return;
        const m = e && typeof e === "object" && "message" in e ? String((e as { message: unknown }).message) : String(e);
        setError(m);
      })
      .finally(() => live && setLoading(false));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);
  return { data, error, loading, reload: () => setNonce((n) => n + 1) };
}
