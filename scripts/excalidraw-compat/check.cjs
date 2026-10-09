// Test-only oracle for #478: AGPL code is fetched at test time and not distributed.
// Loads every file the Tessera writer tests produced (TESSERA_EXCALIDRAW_DUMP)
// with the Obsidian Excalidraw plugin's own ExcalidrawData.loadData
// (https://github.com/zsviczian/obsidian-excalidraw-plugin, bundled by build.mjs)
// and checks that the plugin sees exactly the text and links Tessera wrote.
// The plugin treats `## Text Elements` as authoritative over the JSON, so a stale
// entry would silently revert an edit; this is the check that catches it.
// Usage: node check.cjs ORACLE_BUNDLE DUMP_DIR
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const stub = require("./stub.cjs");

// Browser and Obsidian globals the plugin modules touch at load time. A new
// ReferenceError means upstream changed; extend this list deliberately.
for (const name of ["excalidrawLib", "mainDocument", "PLUGIN_VERSION", "document", "navigator", "app"]) {
  if (!(name in globalThis)) globalThis[name] = stub;
}
globalThis.window = globalThis;
const { ExcalidrawData } = require(path.resolve(process.argv[2]));
const dir = path.resolve(process.argv[3]);

// Explicit values wherever loadData's behaviour depends on them; stub elsewhere.
const withStub = (object) => new Proxy(object, { get: (t, k) => (k in t ? t[k] : stub[k]) });
const plugin = withStub({
  settings: withStub({
    syncExcalidraw: false,
    compress: true,
    decompressForMDView: false,
    addDummyTextElement: false,
    showLinkBrackets: true,
    linkPrefix: "",
    urlPrefix: "",
    parseTODO: false,
  }),
  app: withStub({
    vault: withStub({ getAbstractFileByPath: () => null, read: async () => "" }),
    metadataCache: withStub({ getFileCache: () => null, getFirstLinkpathDest: () => null }),
  }),
});

const sorted = (entries) => JSON.stringify(Object.fromEntries([...entries].sort(([a], [b]) => (a < b ? -1 : 1))));

(async () => {
  const files = fs.readdirSync(dir).filter((f) => f.endsWith(".excalidraw.md")).sort();
  let checked = 0;
  let normalised = 0;
  const failures = [];
  for (const name of files) {
    const raw = fs.readFileSync(path.join(dir, name), "utf8");
    if (raw.replace(/^﻿/, "").trimStart().startsWith("{")) continue; // plain .excalidraw JSON
    // The writer keeps the opened file's line endings. The plugin's section
    // regexes expect "\n" and its loadData rejects a CRLF Drawing block, so CRLF
    // input files are checked after normalisation (as the Rust check does) and
    // counted separately; whether Obsidian normalises on read is not asserted.
    const crlf = raw.includes("\r\n");
    if (crlf) normalised += 1;
    const data = crlf ? raw.replace(/\r\n/g, "\n") : raw;
    const expected = JSON.parse(fs.readFileSync(path.join(dir, name.replace(".excalidraw.md", ".expected.json")), "utf8"));
    const loaded = new ExcalidrawData(plugin);
    const file = { path: name, basename: name.replace(/\.md$/, ""), extension: "md", stat: { mtime: 1 } };
    try {
      if (!(await loaded.loadData(data, file, "raw"))) throw new Error("loadData returned false");
      const text = sorted([...loaded.textElements].map(([id, value]) => [id, value.raw]));
      const links = sorted(loaded.elementLinks);
      if (text !== sorted(Object.entries(expected.text))) {
        throw new Error(`text elements differ\n  plugin:   ${text}\n  expected: ${sorted(Object.entries(expected.text))}`);
      }
      if (links !== sorted(Object.entries(expected.links))) {
        throw new Error(`element links differ\n  plugin:   ${links}\n  expected: ${sorted(Object.entries(expected.links))}`);
      }
    } catch (error) {
      failures.push(`${name}: ${error && error.message}`);
    }
    checked += 1;
  }
  // Positive control: the same loader must see a stale `## Text Elements` entry
  // as different from the JSON, or the comparisons above prove nothing.
  let control = "no single-line text entry to corrupt";
  for (const name of files) {
    const data = fs.readFileSync(path.join(dir, name), "utf8").replace(/\r\n/g, "\n");
    const expected = JSON.parse(fs.readFileSync(path.join(dir, name.replace(".excalidraw.md", ".expected.json")), "utf8"));
    const entry = Object.entries(expected.text).find(([id, text]) => !text.includes("\n") && data.includes(`\n${text} ^${id}\n`));
    if (!entry) continue;
    const [id, text] = entry;
    const stale = data.replace(`\n${text} ^${id}\n`, `\nSTALE ${text} ^${id}\n`);
    const loaded = new ExcalidrawData(plugin);
    await loaded.loadData(stale, { path: name, basename: name, extension: "md", stat: { mtime: 1 } }, "raw");
    const seen = loaded.textElements.get(id)?.raw;
    control = seen === `STALE ${text}` ? null : `stale entry not seen (got ${JSON.stringify(seen)})`;
    break;
  }
  if (control) failures.push(`positive control: ${control}`);
  else console.log("positive control: a stale Text Elements entry is detected");
  console.log(`plugin oracle: ${checked} Markdown drawings checked (${normalised} CRLF normalised), ${failures.length} failed`);
  for (const failure of failures) console.log(`FAIL ${failure}`);
  // An empty dump would make this check vacuous.
  if (checked === 0) {
    console.log("FAIL no Markdown drawings were dumped");
    process.exit(1);
  }
  process.exit(failures.length ? 1 : 0);
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
