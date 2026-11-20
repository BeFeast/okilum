// What the page counts as an unsaved change (#478). Pure, so the transitions
// the editor goes through are tested without a browser.
//
// A fingerprint covers the elements (Excalidraw's scene version, which every
// element edit and deletion bumps) and the canvas settings the writer keeps
// in the file. Pan and zoom are not changes.
//
// Only the snapshot Okilum actually wrote becomes the saved state: the canvas
// stays editable while a call is in flight, so anything drawn after the
// snapshot was taken is still unsaved when the reply arrives.

export const PERSISTENT_APP_STATE = ["viewBackgroundColor", "gridSize", "gridStep", "gridModeEnabled"];

export function fingerprint(sceneVersion, appState = {}) {
  return JSON.stringify([sceneVersion, ...PERSISTENT_APP_STATE.map((key) => appState[key] ?? null)]);
}

export class Tracker {
  constructor() {
    this.saved = null;
    // The snapshot the file holds in a pending conflict: what Keep mine
    // writes and Save as copy copies.
    this.conflicted = null;
  }

  // The scene the editor holds right after a load is the file.
  load(snapshot) {
    this.saved = snapshot;
    this.conflicted = null;
  }

  loaded() {
    return this.saved !== null;
  }

  // Okilum wrote `snapshot`: Apply saved, or Keep mine wrote the conflict's.
  wrote(snapshot) {
    this.saved = snapshot;
    this.conflicted = null;
  }

  // Apply of `snapshot` hit a conflict; nothing was written.
  conflict(snapshot) {
    this.conflicted = snapshot;
  }

  keptMine() {
    this.wrote(this.conflicted);
  }

  dirty(current) {
    return this.saved !== null && current !== this.saved;
  }

  // Edits during an open conflict would not reach Keep mine or Save as copy,
  // which use the scene that hit the conflict, so the canvas is read-only.
  editable() {
    return this.conflicted === null;
  }
}
