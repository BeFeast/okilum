// Review of #3183: the real command handlers with delayed Okilum replies.
// The fake canvas accepts an edit only while the controller says the canvas
// is editable, as Excalidraw's viewModeEnabled does.
import assert from "node:assert/strict";
import test from "node:test";
import { Controller, DISCARD } from "../src/controller.js";
import { Session } from "../src/session.js";
import { fingerprint } from "../src/tracker.js";

function harness({ answer = true } = {}) {
  const pending = [];
  const fetchImpl = (url, init) =>
    new Promise((resolve) =>
      pending.push({ action: url.split("/api/")[1], body: init.body && JSON.parse(init.body), resolve }),
    );
  const reply = async (status, data) => {
    while (!pending.length) await new Promise((resolve) => setTimeout(resolve, 0));
    const call = pending.shift();
    call.resolve({ ok: status < 300, status, json: async () => data });
    await new Promise((resolve) => setTimeout(resolve, 0));
    return call;
  };
  const canvas = { version: 1, mounted: true, shows: 0 };
  const asked = [];
  const session = new Session("/t/", fetchImpl);
  const controller = new Controller({
    session,
    confirm: (text) => {
      asked.push(text);
      return answer;
    },
    editor: {
      state: () => (canvas.mounted ? fingerprint(canvas.version, {}) : null),
      scene: () => ({ elements: [canvas.version] }),
    },
    view: {
      update() {},
      show() {
        canvas.shows += 1;
        canvas.version = 100 + canvas.shows; // the scene Okilum sent
      },
      closed() {
        canvas.mounted = false;
      },
    },
  });
  const draw = () => {
    if (!controller.editable()) return false;
    canvas.version += 1;
    return true;
  };
  return { controller, session, canvas, draw, reply, asked, pending };
}

// Load, then Apply one edit so Undo Apply is available.
async function applied(h) {
  const loading = h.controller.loaded({}, fingerprint(h.canvas.version, {}));
  await h.reply(200, { status: "ready", revision: 0, canUndo: false });
  await loading;
  assert.ok(h.draw());
  const applying = h.controller.apply();
  await h.reply(200, { status: "saved", revision: 1, canUndo: true });
  await applying;
  assert.equal(h.controller.dirty(), false);
}

test("Undo Apply with unsaved edits asks, and keeps them when declined", async () => {
  const h = harness({ answer: false });
  await applied(h);
  assert.ok(h.draw()); // B, not applied
  await h.controller.undo();
  assert.deepEqual(h.asked, [DISCARD.undo]);
  assert.equal(h.pending.length, 0, "nothing was sent");
  assert.equal(h.canvas.shows, 0);
  assert.equal(h.controller.dirty(), true, "B is still on the canvas, unsaved");
  assert.equal(h.controller.editable(), true);
});

test("the canvas is frozen while Undo Apply is in flight", async () => {
  const h = harness();
  await applied(h);
  const undoing = h.controller.undo();
  assert.deepEqual(h.asked, [], "a clean canvas needs no confirmation");
  assert.equal(h.draw(), false, "no edit can land in the scene Undo replaces");
  await h.reply(200, { status: "restored", revision: 2, canUndo: false, scene: {} });
  await undoing;
  assert.equal(h.canvas.shows, 1);
  assert.equal(h.controller.editable(), true, "the restored scene is editable again");
});

test("a blocked Undo unfreezes and keeps the canvas", async () => {
  const h = harness();
  await applied(h);
  const undoing = h.controller.undo();
  assert.equal(h.draw(), false);
  await h.reply(409, { status: "blocked", revision: 1, canUndo: false });
  await undoing;
  assert.equal(h.canvas.shows, 0);
  assert.equal(h.controller.editable(), true);
  assert.match(h.controller.error, /Undo did nothing/);
  assert.ok(h.draw());
});

test("Done freezes the canvas until the session has ended", async () => {
  const h = harness();
  await applied(h);
  const closing = h.controller.close();
  assert.equal(h.draw(), false, "no edit can be drawn into a closing session");
  await h.reply(200, { status: "closed", revision: 1 });
  await closing;
  assert.equal(h.canvas.mounted, false);
  assert.deepEqual(h.asked, []);
});

test("Done with unsaved edits asks; declining keeps editing, a failure unfreezes", async () => {
  const h = harness({ answer: false });
  await applied(h);
  assert.ok(h.draw());
  await h.controller.close();
  assert.deepEqual(h.asked, [DISCARD.close]);
  assert.equal(h.pending.length, 0);
  assert.equal(h.canvas.mounted, true);
  const ok = harness();
  await applied(ok);
  const closing = ok.controller.close();
  await ok.reply(500, { error: "disk full" });
  await closing;
  assert.equal(ok.canvas.mounted, true);
  assert.equal(ok.controller.editable(), true);
  assert.match(ok.controller.error, /disk full/);
});

test("Reload from a conflict asks before discarding the conflicting edit", async () => {
  const h = harness({ answer: false });
  const loading = h.controller.loaded({}, fingerprint(h.canvas.version, {}));
  await h.reply(200, { status: "ready", revision: 0 });
  await loading;
  h.draw();
  const applying = h.controller.apply();
  await h.reply(409, { status: "conflict", revision: 0, conflict: "d1" });
  await applying;
  assert.equal(h.draw(), false, "read-only during the conflict");
  await h.controller.reload();
  assert.deepEqual(h.asked, [DISCARD.reload]);
  assert.equal(h.pending.length, 0);
  assert.equal(h.controller.conflict, true);
});

test("an edit made while Apply is in flight stays unsaved (handler level)", async () => {
  const h = harness();
  const loading = h.controller.loaded({}, fingerprint(h.canvas.version, {}));
  await h.reply(200, { status: "ready", revision: 0 });
  await loading;
  h.draw();
  const applying = h.controller.apply();
  assert.ok(h.draw(), "Apply keeps the canvas editable");
  await h.reply(200, { status: "saved", revision: 1, canUndo: true });
  await applying;
  assert.equal(h.controller.dirty(), true);
});
