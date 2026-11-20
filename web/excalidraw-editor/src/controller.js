// The editor page's commands (#478), without React, so every transition is
// tested with delayed Okilum replies.
//
// Rules:
// - Only the snapshot Okilum wrote counts as saved (see tracker.js). Apply
//   and Keep mine leave the canvas editable; later edits stay unsaved.
// - A command that replaces the scene or ends the session (Undo Apply,
//   Reload, Done) asks before discarding unsaved edits and freezes the canvas
//   for the whole request, so nothing can be drawn into a scene that is about
//   to be replaced. A refused or failed request unfreezes it again.
// - During an open conflict the canvas is read-only.
import { SessionError } from "./session.js";
import { Tracker } from "./tracker.js";

export const DISCARD = {
  undo: "Undo the last Apply? Changes you have not applied will be lost.",
  reload: "Load the drawing from the vault? Changes you have not applied will be lost.",
  close: "Close without applying your changes?",
};

function message(error) {
  return error instanceof SessionError || error instanceof Error ? error.message : String(error);
}

export class Controller {
  // editor: { state(): fingerprint | null, scene(): object }
  // view: { update(): void, show(data): void, closed(): void }
  constructor({ session, editor, view, confirm }) {
    this.session = session;
    this.editor = editor;
    this.view = view;
    this.confirm = confirm;
    this.tracker = new Tracker();
    this.frozen = false;
    this.conflict = false;
    this.status = "";
    this.error = "";
    this.copy = "";
  }

  editable() {
    return !this.frozen && this.tracker.editable();
  }

  dirty() {
    const state = this.editor.state();
    return state !== null && this.tracker.dirty(state);
  }

  // A new scene from Okilum is on its way into the editor.
  reset() {
    this.tracker = new Tracker();
    this.conflict = false;
    this.copy = "";
  }

  // The editor holds the loaded scene; `snapshot` is its state then.
  async loaded(scene, snapshot) {
    await this.run(async () => {
      await this.session.loaded(scene);
      this.tracker.load(snapshot);
      this.status = "";
    });
  }

  async run(work) {
    this.error = "";
    try {
      await work();
    } catch (error) {
      this.error = message(error);
    }
    this.view.update();
  }

  // Scene-replacing commands: ask, freeze, and unfreeze unless replaced.
  async replace(kind, work) {
    if (this.frozen) return;
    if (this.dirty() && !this.confirm(DISCARD[kind])) return;
    this.frozen = true;
    this.view.update();
    let replaced = false;
    await this.run(async () => {
      replaced = await work();
    });
    if (!replaced) {
      this.frozen = false;
      this.view.update();
    }
  }

  // Okilum sent a scene for the editor to load; the canvas comes back
  // editable with the new load.
  shown(data) {
    this.reset();
    this.frozen = false;
    this.view.show(data);
  }

  apply() {
    return this.run(async () => {
      if (!this.tracker.loaded() || this.frozen) return;
      const snapshot = this.editor.state();
      const data = await this.session.apply(this.editor.scene());
      if (data.status === "conflict") {
        this.tracker.conflict(snapshot);
        this.conflict = true;
        this.status = "";
        return;
      }
      this.tracker.wrote(snapshot);
      this.status = data.status === "unchanged" ? "Nothing to apply" : "Applied to the drawing";
    });
  }

  keepMine() {
    return this.run(async () => {
      const data = await this.session.keepMine();
      if (data.status === "conflict") {
        this.error = "The drawing changed again. Look at it in Okilum, then choose again.";
        return;
      }
      // Keep mine writes the scene that hit the conflict, not the canvas.
      this.tracker.keptMine();
      this.conflict = false;
      this.status = "Applied to the drawing";
    });
  }

  saveCopy() {
    return this.run(async () => {
      const data = await this.session.saveCopy();
      this.copy = data.copy || "";
    });
  }

  undo() {
    return this.replace("undo", async () => {
      const data = await this.session.undo();
      if (data.status === "blocked") {
        this.status = "";
        this.error = "The drawing changed outside this editor since the last Apply, so Undo did nothing.";
        return false;
      }
      this.shown(data);
      this.status = "Last Apply undone";
      return true;
    });
  }

  reload() {
    return this.replace("reload", async () => {
      const data = await this.session.reload();
      this.shown(data);
      this.status = "Loaded the drawing from the vault";
      return true;
    });
  }

  close() {
    return this.replace("close", async () => {
      await this.session.close();
      this.view.closed();
      return true;
    });
  }
}
