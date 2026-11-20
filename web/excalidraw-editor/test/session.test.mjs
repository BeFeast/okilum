// The page's session client (#478): revision bookkeeping, conflict digest,
// refusal of overlapping calls and of calls after the session ended.
import assert from "node:assert/strict";
import test from "node:test";
import { Session, SessionError, sessionBase } from "../src/session.js";

function fake(replies) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init, body: init.body ? JSON.parse(init.body) : undefined });
    const [status, data] = replies.shift();
    return { ok: status >= 200 && status < 300, status, json: async () => data };
  };
  return { calls, fetchImpl };
}

test("only a 64-hex capability folder is a session", () => {
  const token = "ab".repeat(32);
  assert.equal(sessionBase({ pathname: `/${token}/` }), `/${token}/`);
  assert.equal(sessionBase({ pathname: `/${token}/index.html` }), `/${token}/`);
  assert.equal(sessionBase({ pathname: "/" }), null);
  assert.equal(sessionBase({ pathname: `/${token.slice(1)}/` }), null);
  assert.equal(sessionBase({ pathname: `/${token.toUpperCase()}/` }), null);
});

test("every call carries the last revision Okilum sent", async () => {
  const { calls, fetchImpl } = fake([
    [200, { revision: 0, canUndo: false, conflict: null, name: "Board", scene: { elements: [] } }],
    [200, { status: "ready", revision: 0, canUndo: false }],
    [200, { status: "saved", revision: 1, canUndo: true, conflict: null }],
    [200, { status: "restored", revision: 2, canUndo: false, scene: { elements: [] } }],
  ]);
  const session = new Session("/t/", fetchImpl);
  await session.open();
  assert.equal(session.name, "Board");
  await session.loaded({ elements: [] });
  await session.apply({ elements: [1] });
  assert.equal(session.canUndo, true);
  await session.undo();
  assert.deepEqual(
    calls.map((call) => [call.url, call.init.method, call.body?.revision]),
    [
      ["/t/api/session", "GET", undefined],
      ["/t/api/loaded", "POST", 0],
      ["/t/api/apply", "POST", 0],
      ["/t/api/undo", "POST", 1],
    ],
  );
  assert.equal(session.revision, 2);
  assert.equal(calls[2].init.headers["Content-Type"], "application/json");
});

test("a conflict is a reply, and Keep mine names the version it showed", async () => {
  const { calls, fetchImpl } = fake([
    [409, { status: "conflict", revision: 0, conflict: "d1" }],
    [409, { status: "conflict", revision: 0, conflict: "d2" }],
    [200, { status: "saved", revision: 1, conflict: null }],
  ]);
  const session = new Session("/t/", fetchImpl);
  const reply = await session.apply({ elements: [] });
  assert.equal(reply.status, "conflict");
  assert.equal(session.conflict, "d1");
  await session.keepMine();
  assert.equal(calls[1].body.reviewed, "d1");
  assert.equal(session.conflict, "d2");
  await session.keepMine();
  assert.equal(calls[2].body.reviewed, "d2");
  assert.equal(session.conflict, null);
});

test("refusals throw with Okilum's message; an ended session refuses locally", async () => {
  const { calls, fetchImpl } = fake([
    [422, { error: "The drawing changed since the editor loaded it. Load it again." }],
    [410, { error: "This editing session has ended." }],
  ]);
  const session = new Session("/t/", fetchImpl);
  await assert.rejects(session.apply({}), (error) => error instanceof SessionError && /Load it again/.test(error.message));
  await assert.rejects(session.apply({}), /ended/);
  assert.equal(session.closed, true);
  await assert.rejects(session.apply({}), (error) => error.status === 410);
  assert.equal(calls.length, 2);
});

test("a second click while a call is in flight is refused, not sent", async () => {
  let release;
  const fetchImpl = async () => {
    await new Promise((resolve) => (release = resolve));
    return { ok: true, status: 200, json: async () => ({ status: "saved", revision: 1 }) };
  };
  const session = new Session("/t/", fetchImpl);
  const first = session.apply({});
  await assert.rejects(session.apply({}), /Wait for the last drawing action/);
  release();
  await first;
  assert.equal(session.revision, 1);
});

test("Okilum unreachable keeps the edit in the tab", async () => {
  const session = new Session("/t/", async () => {
    throw new TypeError("Failed to fetch");
  });
  await assert.rejects(session.apply({}), /still in this tab/);
});
