// Okilum's offline Excalidraw editor for one drawing (#478). The scene comes
// from the Okilum session that opened this tab; changes reach the vault file
// only through an explicit Apply, which Okilum writes with the same
// revision-checked save it uses for notes.
import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { Excalidraw, getSceneVersion, serializeAsJSON } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import "./editor.css";
import { Session, SessionError, sessionBase } from "./session.js";
import { Tracker, fingerprint } from "./tracker.js";

const base = sessionBase(window.location);
const session = base ? new Session(base) : null;

function stateOf(api) {
  return fingerprint(getSceneVersion(api.getSceneElementsIncludingDeleted()), api.getAppState());
}

function sceneOf(api) {
  return JSON.parse(
    serializeAsJSON(api.getSceneElementsIncludingDeleted(), api.getAppState(), api.getFiles(), "local"),
  );
}

// Excalidraw restores initialData asynchronously; the baseline is the scene it
// holds once that is done, so its load normalisation never counts as an edit.
async function settled(api) {
  for (let i = 0; i < 200 && api.getAppState().isLoading; i++) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}

function message(error) {
  return error instanceof SessionError || error instanceof Error ? error.message : String(error);
}

function App() {
  const [phase, setPhase] = useState(session ? "opening" : "invalid");
  const [scene, setScene] = useState(null);
  const [load, setLoad] = useState(0);
  const [api, setApi] = useState(null);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [dirty, setDirty] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [copy, setCopy] = useState("");
  const [, setTick] = useState(0);
  const tracker = useRef(new Tracker());
  const refresh = () => setTick((tick) => tick + 1);

  const show = useCallback((data) => {
    if (data.name) document.title = `${data.name} · Okilum`;
    setScene(data.scene);
    setLoad((count) => count + 1);
    setApi(null);
    tracker.current = new Tracker();
    setDirty(false);
    setConflict(false);
    setCopy("");
    setPhase("editing");
  }, []);

  useEffect(() => {
    if (!session) return;
    session
      .open()
      .then((data) => {
        if (data.loaded) {
          if (data.name) document.title = `${data.name} · Okilum`;
          setPhase("elsewhere");
        } else {
          show(data);
        }
      })
      .catch((e) => {
        setError(message(e));
        setPhase("failed");
      });
  }, [show]);

  // Report the loaded scene once per load.
  useEffect(() => {
    if (!api || tracker.current.loaded()) return;
    let cancelled = false;
    (async () => {
      await settled(api);
      if (cancelled) return;
      const loaded = sceneOf(api);
      const snapshot = stateOf(api);
      try {
        await session.loaded(loaded);
        tracker.current.load(snapshot);
        setDirty(tracker.current.dirty(stateOf(api)));
        setStatus("");
        refresh();
      } catch (e) {
        setError(message(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, load]);

  useEffect(() => {
    const warn = (event) => {
      if (dirty && !session?.closed) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);

  const onChange = useCallback((elements, appState) => {
    setDirty(tracker.current.dirty(fingerprint(getSceneVersion(elements), appState)));
  }, []);

  const act = async (work) => {
    setError("");
    try {
      await work();
    } catch (e) {
      setError(message(e));
    }
    refresh();
  };

  const apply = () =>
    act(async () => {
      if (!api || !tracker.current.loaded()) return;
      // The canvas stays editable while the call is in flight: only this
      // snapshot is what Okilum writes.
      const snapshot = stateOf(api);
      const data = await session.apply(sceneOf(api));
      if (data.status === "conflict") {
        tracker.current.conflict(snapshot);
        setConflict(true);
        setDirty(tracker.current.dirty(stateOf(api)));
        setStatus("");
        return;
      }
      tracker.current.wrote(snapshot);
      setDirty(tracker.current.dirty(stateOf(api)));
      setStatus(data.status === "unchanged" ? "Nothing to apply" : "Applied to the drawing");
    });
  const undo = () =>
    act(async () => {
      const data = await session.undo();
      if (data.status === "blocked") {
        setStatus("");
        setError("The drawing changed outside this editor since the last Apply, so Undo did nothing.");
        return;
      }
      show(data);
      setStatus("Last Apply undone");
    });
  const keepMine = () =>
    act(async () => {
      const data = await session.keepMine();
      if (data.status === "conflict") {
        setError("The drawing changed again. Look at it in Okilum, then choose again.");
        return;
      }
      // Keep mine writes the scene that hit the conflict, not the canvas.
      tracker.current.keptMine();
      setDirty(tracker.current.dirty(stateOf(api)));
      setConflict(false);
      setStatus("Applied to the drawing");
    });
  const reload = () =>
    act(async () => {
      const data = await session.reload();
      show(data);
      setStatus("Loaded the drawing from the vault");
    });
  const saveCopy = () =>
    act(async () => {
      const data = await session.saveCopy();
      setCopy(data.copy || "");
    });
  const close = () =>
    act(async () => {
      const unsaved = api ? tracker.current.dirty(stateOf(api)) : dirty;
      if (unsaved && !window.confirm("Close without applying your changes?")) return;
      await session.close();
      setDirty(false);
      setPhase("closed");
    });

  if (phase === "invalid") return <Notice title="This editor link is not valid">Open the drawing from Okilum.</Notice>;
  if (phase === "opening") return <Notice title="Preparing your drawing…" />;
  if (phase === "failed") return <Notice title="The drawing could not be opened">{error}</Notice>;
  if (phase === "closed") return <Notice title="Editing ended">You can close this tab.</Notice>;
  if (phase === "elsewhere") {
    return (
      <Notice title="This drawing is open in another tab">
        <p>Edit it here instead? The other tab can no longer apply its changes.</p>
        <button className="okl-button okl-primary" onClick={reload}>Edit here</button>
        {error && <p className="okl-error">{error}</p>}
      </Notice>
    );
  }

  const ready = api !== null && tracker.current.loaded() && !session.closed;
  return (
    <div className="okl-editor">
      <Excalidraw
        key={load}
        initialData={{ ...scene, scrollToContent: true }}
        excalidrawAPI={setApi}
        onChange={onChange}
        viewModeEnabled={!tracker.current.editable()}
        name={session.name}
        UIOptions={{ canvasActions: { loadScene: false, saveToActiveFile: false } }}
        renderTopRightUI={() => (
          <div className="okl-actions">
            <button className="okl-button okl-primary" disabled={!ready || session.busy || conflict} onClick={apply}>
              {dirty ? "Apply •" : "Apply"}
            </button>
            {session.canUndo && !conflict && (
              <button className="okl-button" disabled={session.busy} onClick={undo}>Undo Apply</button>
            )}
            <button className="okl-button" disabled={session.busy} onClick={close}>Done</button>
          </div>
        )}
      />
      {(status || error || conflict) && (
        <div className="okl-bar" role="status">
          {conflict ? (
            <>
              <span>The drawing changed outside this editor. Your edit was not applied, and the canvas is read-only until you choose.</span>
              <button className="okl-button" disabled={session.busy} onClick={reload}>Reload</button>
              <button className="okl-button" disabled={session.busy} onClick={keepMine}>Keep mine</button>
              <button className="okl-button" disabled={session.busy} onClick={saveCopy}>Save as copy</button>
              {copy && <span>Saved as {copy}</span>}
            </>
          ) : (
            status && <span>{status}</span>
          )}
          {error && <span className="okl-error">{error}</span>}
        </div>
      )}
    </div>
  );
}

function Notice({ title, children }) {
  return (
    <main className="okl-notice">
      <h1>{title}</h1>
      {typeof children === "string" ? <p>{children}</p> : children}
    </main>
  );
}

createRoot(document.getElementById("root")).render(<App />);
