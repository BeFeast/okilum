# Okilum drawing editor page (#478)

The offline Excalidraw page that Edit drawing opens. Okilum serves it on
`127.0.0.1` for one drawing at a time; `src/session.js` is the page's side of
that session and `crates/okilum-shell/src/reader_drawing_edit.rs` the other.
Behaviour and guards: `docs/excalidraw-reader.md`, section Edit drawing.

The build output lives in `crates/okilum-shell/assets/excalidraw-editor/` and is
committed, so desktop builds need no Node. After changing anything here:

```bash
npm ci --ignore-scripts
node build.mjs           # rewrite the committed output
node build.mjs --check   # must report a byte-identical rebuild
node --test test/
```

Upgrading `@excalidraw/excalidraw`: change the pin in `package.json`, run
`npm install --ignore-scripts`, rebuild, and check the round trip on real
drawings (Apply, Undo Apply, a conflict) before committing.
