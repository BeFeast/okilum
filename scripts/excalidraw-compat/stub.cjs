// Test-only oracle for #478: AGPL code is fetched at test time and not distributed.
// This file is Okilum's own (MIT): a recursive stand-in for the Obsidian API and
// every plugin module the oracle does not exercise. Upstream:
// https://github.com/zsviczian/obsidian-excalidraw-plugin
//
// Any property is another stub, calls and `new` return stubs, and it is not
// thenable (so an `await` on it does not hang). It never supplies data: values
// the loader actually depends on are given explicitly in check.cjs.
const make = (name) =>
  new Proxy(function () {}, {
    get: (_, key) =>
      key === "then"
        ? undefined
        : key === "__esModule"
          ? true
          : key === Symbol.toPrimitive
            ? () => name
            : make(`${name}.${String(key)}`),
    apply: () => make(`${name}()`),
    construct: () => make(`new ${name}`),
  });
module.exports = make("stub");
