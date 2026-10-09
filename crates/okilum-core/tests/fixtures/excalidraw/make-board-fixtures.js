// Builds the board*.excalidraw.md fixtures in the Obsidian Excalidraw plugin's
// layout (generateMDBase / getMarkdownDrawingSection) with real JS lz-string,
// so the Rust encoder and writer are checked against the plugin's own bytes.
//
// Usage (outside the repo): npm install lz-string@1.5.0, then
//   node make-board-fixtures.js <path>/node_modules/lz-string <this directory>
const LZString = require(process.argv[2]);
const fs = require("fs");
const out = process.argv[3];
const base = (id, type, x, y, w, h, seed, extra = {}) => ({
  id, type, x, y, width: w, height: h, angle: 0,
  strokeColor: "#1e1e1e", backgroundColor: "transparent", fillStyle: "solid",
  strokeWidth: 2, strokeStyle: "solid", roughness: 1, opacity: 100,
  groupIds: [], frameId: null, index: extra.index, roundness: null, seed,
  version: 12, versionNonce: seed * 7 % 2147483647, isDeleted: false,
  boundElements: null, updated: 1759800000000 + seed, link: null, locked: false,
  ...extra,
});
const text = (id, x, y, raw, display, seed, extra = {}) => base(id, "text", x, y, 180, 25 * display.split("\n").length, seed, {
  fontSize: 20, fontFamily: 5, text: display, rawText: raw, textAlign: "left",
  verticalAlign: "top", containerId: null, originalText: display, autoResize: true,
  lineHeight: 1.25, ...extra,
});
const elements = [
  base("Rr1aBcD3", "rectangle", 0, 0, 200, 80, 101, { index: "a0", backgroundColor: "#a5d8ff", roundness: { type: 3 },
    boundElements: [{ type: "text", id: "Tx1aBcD3" }, { type: "arrow", id: "Ar1aBcD3" }] }),
  text("Tx1aBcD3", 70, 27.5, "Inbox", "Inbox", 102, { index: "a1", textAlign: "center", verticalAlign: "middle", containerId: "Rr1aBcD3" }),
  text("Tx2kLmN4", 0, 140, "See [[Project Plan|the plan]]", "See the plan", 103, { index: "a2" }),
  text("Tx3pQrS5", 0, 200, "Line one\nLine two", "Line one\nLine two", 104, { index: "a3" }),
  base("El1wXyZ6", "ellipse", 320, 0, 120, 80, 105, { index: "a4", link: "[[Daily Note]]",
    boundElements: [{ type: "arrow", id: "Ar1aBcD3" }] }),
  base("Ar1aBcD3", "arrow", 204, 40, 112, 0.5, 106, { index: "a5", roundness: { type: 2 },
    points: [[0, 0], [112, 0.5]], lastCommittedPoint: null,
    startBinding: { elementId: "Rr1aBcD3", focus: 0.02, gap: 4 },
    endBinding: { elementId: "El1wXyZ6", focus: -0.01, gap: 4.5 },
    startArrowhead: null, endArrowhead: "arrow", elbowed: false }),
  base("Im1gHjK7", "image", 0, 280, 160, 120, 107, { index: "a6", status: "saved",
    fileId: "5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a", scale: [1, 1], crop: null }),
  base("Im2LaTeX", "image", 200, 280, 90, 30, 108, { index: "a7", status: "saved",
    fileId: "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d", scale: [1, 1], crop: null }),
  { ...text("TxDeadOn", 400, 200, "Old idea", "Old idea", 109, { index: "a8" }), isDeleted: true, version: 15 },
];
const scene = {
  type: "excalidraw", version: 2,
  source: "https://github.com/zsviczian/obsidian-excalidraw-plugin/releases/tag/2.15.3",
  elements,
  appState: { theme: "light", viewBackgroundColor: "#ffffff", currentItemStrokeColor: "#1e1e1e",
    currentItemFontFamily: 5, gridSize: 20, gridStep: 5, gridModeEnabled: false, objectsSnapModeEnabled: false },
  files: {},
};
const md = (compressed) => {
  const json = JSON.stringify(scene, null, "\t");
  let drawing;
  if (compressed) {
    const c = LZString.compressToBase64(json);
    let r = "";
    for (let i = 0; i < c.length; i += 256) r += c.slice(i, i + 256) + "\n\n";
    drawing = "## Drawing\n```compressed-json\n" + r.trim() + "\n```\n%%";
  } else {
    drawing = "## Drawing\n```json\n" + json + "\n```\n%%";
  }
  return `---

excalidraw-plugin: parsed
tags: [excalidraw]
aliases: [Planning board]

---
==⚠  Switch to EXCALIDRAW VIEW in the MORE OPTIONS menu of this document. ⚠== You can decompress Drawing data with the command palette: 'Decompress current Excalidraw file'. For more info check in plugin settings under 'Saving'

Back of the note: the board for the [[Project Plan]] review.

- [ ] follow up with design

# Excalidraw Data

## Text Elements
Inbox ^Tx1aBcD3

See [[Project Plan|the plan]] ^Tx2kLmN4

Line one
Line two ^Tx3pQrS5

## Element Links
El1wXyZ6: [[Daily Note]]

## Embedded Files
5f4c2a0d9b8e7f6a1c3b5d7e9f0a2c4e6b8d0f1a: [[assets/diagram.png]]

9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d: $$E = mc^2$$

%%
${drawing}
`;
};
fs.writeFileSync(`${out}/board.excalidraw.md`, md(true));
fs.writeFileSync(`${out}/board-json.excalidraw.md`, md(false));
