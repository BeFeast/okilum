// Review of #3183: the page must never call a scene saved that Okilum did not
// write. These replay the editor's transitions with real session replies.
import assert from "node:assert/strict";
import test from "node:test";
import { PERSISTENT_APP_STATE, Tracker, fingerprint } from "../src/tracker.js";
import { Session } from "../src/session.js";

const plain = { viewBackgroundColor: "#ffffff", gridSize: 20, gridStep: 5, gridModeEnabled: false };

// A reply Okilum holds until the test releases it, as a slow Apply would.
function delayed() {
  const pending = [];
  const fetchImpl = (url, init) =>
    new Promise((resolve) => pending.push({ url, body: JSON.parse(init.body), resolve }));
  const reply = (status, data) => {
    const call = pending.shift();
    call.resolve({ ok: status < 300, status, json: async () => data });
    return call;
  };
  return { fetchImpl, reply };
}

test("an edit made while Apply is in flight stays unsaved", async () => {
  const { fetchImpl, reply } = delayed();
  const session = new Session("/t/", fetchImpl);
  const tracker = new Tracker();
  tracker.load(fingerprint(1, plain));
  const a = fingerprint(2, plain); // scene A, what Apply sends
  const applying = session.apply({ elements: ["A"] });
  const b = fingerprint(3, plain); // the user draws B before the reply
  reply(200, { status: "saved", revision: 1, canUndo: true });
  await applying;
  tracker.wrote(a);
  assert.equal(tracker.dirty(b), true, "B is not on disk, so it must stay dirty");
  assert.equal(tracker.dirty(a), false);
  // Control: without the late edit the canvas is clean after Apply.
  const control = new Tracker();
  control.load(fingerprint(1, plain));
  control.wrote(a);
  assert.equal(control.dirty(a), false);
});

test("Keep mine acknowledges the conflicting scene, not later canvas edits", async () => {
  const { fetchImpl, reply } = delayed();
  const session = new Session("/t/", fetchImpl);
  const tracker = new Tracker();
  tracker.load(fingerprint(1, plain));
  const a = fingerprint(2, plain);
  const applying = session.apply({ elements: ["A"] });
  const b = fingerprint(3, plain); // drawn before the conflict reply arrived
  reply(409, { status: "conflict", revision: 0, conflict: "d1" });
  await applying;
  tracker.conflict(a);
  assert.equal(tracker.editable(), false, "the canvas is read-only during a conflict");
  assert.equal(tracker.dirty(b), true);
  const keeping = session.keepMine();
  const sent = reply(200, { status: "saved", revision: 1, conflict: null });
  await keeping;
  assert.deepEqual(Object.keys(sent.body).sort(), ["reviewed", "revision"], "Keep mine sends no scene");
  tracker.keptMine();
  assert.equal(tracker.editable(), true);
  assert.equal(tracker.dirty(b), true, "B was not written by Keep mine");
  assert.equal(tracker.dirty(a), false);
});

test("Save as copy leaves the canvas read-only and the edit unsaved", () => {
  const tracker = new Tracker();
  tracker.load(fingerprint(1, plain));
  tracker.conflict(fingerprint(2, plain));
  // Save as copy writes a new file; the drawing itself is still unchanged.
  assert.equal(tracker.editable(), false);
  assert.equal(tracker.dirty(fingerprint(2, plain)), true);
  // Reload replaces the tracker with a fresh load.
  const reloaded = new Tracker();
  reloaded.load(fingerprint(5, plain));
  assert.equal(reloaded.editable(), true);
});

test("canvas settings the file keeps are changes; pan and zoom are not", () => {
  const tracker = new Tracker();
  tracker.load(fingerprint(1, plain));
  assert.equal(tracker.dirty(fingerprint(1, { ...plain, scrollX: 40, zoom: { value: 2 } })), false);
  for (const key of PERSISTENT_APP_STATE) {
    const changed = { ...plain, [key]: key === "viewBackgroundColor" ? "#ffc9c9" : key === "gridModeEnabled" ? true : 50 };
    assert.equal(tracker.dirty(fingerprint(1, changed)), true, key);
  }
  // A background-only Apply makes it clean again.
  const background = fingerprint(1, { ...plain, viewBackgroundColor: "#ffc9c9" });
  tracker.wrote(background);
  assert.equal(tracker.dirty(background), false);
  assert.deepEqual(PERSISTENT_APP_STATE.sort(), ["gridModeEnabled", "gridSize", "gridStep", "viewBackgroundColor"]);
});

test("nothing is dirty before the editor reported its load", () => {
  assert.equal(new Tracker().dirty(fingerprint(9, plain)), false);
});
