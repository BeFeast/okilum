// Okilum's offline Excalidraw editor for one drawing (#478). The scene comes
// from the Okilum session that opened this tab; changes reach the vault file
// only through an explicit Apply, which Okilum writes with the same
// revision-checked save it uses for notes.
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { Excalidraw, getSceneVersion, serializeAsJSON } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import "./editor.css";
import { Session, SessionError, sessionBase } from "./session.js";
import { fingerprint } from "./tracker.js";
import { Controller } from "./controller.js";

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
  const [dirty, setDirty] = useState(false);
  const [openError, setOpenError] = useState("");
  const [, setTick] = useState(0);
  const apiRef = useRef(null);
  apiRef.current = api;

  const controller = useMemo(
    () =>
      session &&
      new Controller({
        session,
        confirm: (text) => window.confirm(text),
        editor: {
          state: () => (apiRef.current ? stateOf(apiRef.current) : null),
          scene: () => sceneOf(apiRef.current),
        },
        view: {
          update: () => {
            setDirty(controller.dirty());
            setTick((tick) => tick + 1);
          },
          show: (data) => {
            if (data.name) document.title = `${data.name} · Okilum`;
            setScene(data.scene);
            setLoad((count) => count + 1);
            setApi(null);
            apiRef.current = null;
            setDirty(false);
            setPhase("editing");
          },
          closed: () => {
            setDirty(false);
            setPhase("closed");
          },
        },
      }),
    [],
  );

  useEffect(() => {
    if (!session) return;
    session
      .open()
      .then((data) => {
        if (data.loaded) {
          if (data.name) document.title = `${data.name} · Okilum`;
          setPhase("elsewhere");
        } else {
          controller.shown(data);
        }
      })
      .catch((e) => {
        setOpenError(message(e));
        setPhase("failed");
      });
  }, [controller]);

  // Report the loaded scene once per load.
  useEffect(() => {
    if (!api || controller.tracker.loaded()) return;
    let cancelled = false;
    (async () => {
      await settled(api);
      if (cancelled) return;
      await controller.loaded(sceneOf(api), stateOf(api));
    })();
    return () => {
      cancelled = true;
    };
  }, [api, load, controller]);

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

  const onChange = useCallback(
    (elements, appState) => {
      setDirty(controller.tracker.dirty(fingerprint(getSceneVersion(elements), appState)));
    },
    [controller],
  );

  if (phase === "invalid") return <Notice title="This editor link is not valid">Open the drawing from Okilum.</Notice>;
  if (phase === "opening") return <Notice title="Preparing your drawing…" />;
  if (phase === "failed") return <Notice title="The drawing could not be opened">{openError}</Notice>;
  if (phase === "closed") return <Notice title="Editing ended">You can close this tab.</Notice>;
  if (phase === "elsewhere") {
    return (
      <Notice title="This drawing is open in another tab">
        <p>Edit it here instead? The other tab can no longer apply its changes.</p>
        <button className="okl-button okl-primary" onClick={() => controller.reload()}>Edit here</button>
        {controller.error && <p className="okl-error">{controller.error}</p>}
      </Notice>
    );
  }

  const { status, error, conflict, copy } = controller;
  const busy = session.busy || controller.frozen;
  const ready = api !== null && controller.tracker.loaded() && !session.closed;
  return (
    <div className="okl-editor">
      <Excalidraw
        key={load}
        initialData={{ ...scene, scrollToContent: true }}
        excalidrawAPI={setApi}
        onChange={onChange}
        viewModeEnabled={!controller.editable()}
        name={session.name}
        UIOptions={{ canvasActions: { loadScene: false, saveToActiveFile: false } }}
        renderTopRightUI={() => (
          <div className="okl-actions">
            <button className="okl-button okl-primary" disabled={!ready || busy || conflict} onClick={() => controller.apply()}>
              {dirty ? "Apply •" : "Apply"}
            </button>
            {session.canUndo && !conflict && (
              <button className="okl-button" disabled={busy} onClick={() => controller.undo()}>Undo Apply</button>
            )}
            <button className="okl-button" disabled={busy} onClick={() => controller.close()}>Done</button>
          </div>
        )}
      />
      {(status || error || conflict) && (
        <div className="okl-bar" role="status">
          {conflict ? (
            <>
              <span>The drawing changed outside this editor. Your edit was not applied, and the canvas is read-only until you choose.</span>
              <button className="okl-button" disabled={busy} onClick={() => controller.reload()}>Reload</button>
              <button className="okl-button" disabled={busy} onClick={() => controller.keepMine()}>Keep mine</button>
              <button className="okl-button" disabled={busy} onClick={() => controller.saveCopy()}>Save as copy</button>
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
