// Test-only oracle for #478: AGPL code is fetched at test time and not distributed.
// Bundles the plugin's own ExcalidrawData loader (fetched by fetch-plugin.sh,
// https://github.com/zsviczian/obsidian-excalidraw-plugin) together with its real
// Markdown/scene parsing modules. Every other import resolves to stub.cjs.
// The bundle is written outside the repository and must never be published.
// Usage: node build.mjs PLUGIN_DIR OUT_FILE
import * as esbuild from "esbuild";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const plugin = path.resolve(process.argv[2]);
const out = path.resolve(process.argv[3]);
// Modules that decide how a drawing file is read; these run as upstream wrote them.
const real = new Set([
  "src/shared/ExcalidrawData.ts",
  "src/shared/excalidrawMarkdownParsing.ts",
  "src/utils/sceneDataUtils.ts",
  "src/utils/pathUtils.ts",
  "src/shared/TextMode.ts",
  "src/constants/constants.ts",
  "src/constants/safeUrls.ts",
]);
const stub = path.join(here, "stub.cjs");

const oracle = {
  name: "oracle",
  setup(build) {
    build.onResolve({ filter: /.*/ }, (args) => {
      if (args.kind === "entry-point") return undefined;
      if (args.path === "lz-string") {
        return { path: path.join(here, "node_modules/lz-string/libs/lz-string.js") };
      }
      let target = null;
      if (args.path.startsWith("src/")) target = path.join(plugin, args.path);
      else if (args.path.startsWith(".")) target = path.resolve(args.resolveDir, args.path);
      if (target) {
        const rel = path.relative(plugin, target).split(path.sep).join("/");
        for (const ext of ["", ".ts", ".tsx"]) {
          if (real.has(rel + ext)) return { path: target + ext };
        }
      }
      return { path: stub };
    });
  },
};

const result = await esbuild.build({
  entryPoints: [path.join(plugin, "src/shared/ExcalidrawData.ts")],
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20",
  write: false,
  plugins: [oracle],
  logLevel: "warning",
});
for (const file of real) {
  if (!fs.existsSync(path.join(plugin, file))) throw new Error(`upstream file moved: ${file}`);
}
// The Proxy stub has no own keys, so esbuild's ESM interop copy would drop every
// named import from it. Hand stubbed imports the Proxy itself.
const text = result.outputFiles[0].text.replaceAll("__toESM(require_stub())", "require_stub()");
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, text);
console.log(`oracle bundle: ${out}`);
