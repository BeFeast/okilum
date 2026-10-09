// Test-only oracle for #478: AGPL code is fetched at test time and not distributed.
// Tessera's (MIT) deterministic stand-in for `nanoid`'s customAlphabet, used where
// the plugin gives new 8-character ids to elements it indexes itself. Ids start
// with "~", which no Excalidraw or Tessera id uses, so they never collide.
let next = 0;
exports.customAlphabet = (_alphabet, size = 21) => () => `~${String(++next).padStart(size - 1, "0")}`;
