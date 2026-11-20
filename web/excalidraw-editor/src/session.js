// The page's side of one Okilum drawing session (#478). Okilum serves this
// page from a loopback address whose first path segment is the session's
// capability; every call goes to `api/<action>` under that folder and carries
// the session revision the page last received. Okilum refuses a call with any
// other revision, so a page that missed a Reload or Undo cannot overwrite the
// drawing with an old scene.
//
// Nothing here stores the scene in the browser. Closing the tab without Apply
// leaves the drawing file as it was.

export class SessionError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export class Session {
  constructor(base, fetchImpl = globalThis.fetch.bind(globalThis)) {
    this.base = base;
    this.fetch = fetchImpl;
    this.revision = 0;
    this.canUndo = false;
    // Digest of the outside version a conflict showed; Keep mine sends it
    // back so Okilum writes over exactly that version and no later one.
    this.conflict = null;
    this.name = "";
    this.busy = false;
    this.closed = false;
  }

  async call(action, body) {
    if (this.closed) throw new SessionError("This editing session has ended. Open the drawing from Okilum again.", 410);
    const init = body === undefined
      ? { method: "GET", cache: "no-store" }
      : {
          method: "POST",
          cache: "no-store",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        };
    let response;
    try {
      response = await this.fetch(`${this.base}api/${action}`, init);
    } catch {
      throw new SessionError("Okilum is not reachable. Your changes are still in this tab: use Export to keep a copy.", 0);
    }
    let data;
    try {
      data = await response.json();
    } catch {
      throw new SessionError("Okilum sent an unreadable reply.", response.status);
    }
    if (typeof data.revision === "number") this.revision = data.revision;
    if (typeof data.canUndo === "boolean") this.canUndo = data.canUndo;
    if ("conflict" in data) this.conflict = data.conflict ?? null;
    if (typeof data.name === "string") this.name = data.name;
    if (response.status === 410) this.closed = true;
    if (!response.ok && response.status !== 409) {
      throw new SessionError(data.error || `Okilum refused the request (${response.status}).`, response.status);
    }
    return data;
  }

  // Serialise calls: a second click while one is in flight is refused rather
  // than sent with a revision that is about to change.
  async run(action, body) {
    if (this.busy) throw new SessionError("Wait for the last drawing action to finish.", 0);
    this.busy = true;
    try {
      return await this.call(action, body);
    } finally {
      this.busy = false;
    }
  }

  open() {
    return this.run("session");
  }
  loaded(scene) {
    return this.run("loaded", { revision: this.revision, scene });
  }
  apply(scene) {
    return this.run("apply", { revision: this.revision, scene });
  }
  keepMine() {
    return this.run("keep-mine", { revision: this.revision, reviewed: this.conflict });
  }
  reload() {
    return this.run("reload", { revision: this.revision });
  }
  saveCopy() {
    return this.run("save-copy", { revision: this.revision });
  }
  undo() {
    return this.run("undo", { revision: this.revision });
  }
  async close() {
    const data = await this.run("close", { revision: this.revision });
    this.closed = true;
    return data;
  }
}

// The session folder of a page URL: `/<capability>/`.
export function sessionBase(location) {
  const segment = location.pathname.split("/")[1] || "";
  if (!/^[0-9a-f]{64}$/.test(segment)) return null;
  return `/${segment}/`;
}
