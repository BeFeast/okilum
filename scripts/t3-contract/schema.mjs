// Run with cwd at the exact upstream release source, using its Effect dependency.
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { writeFile } from 'node:fs/promises';
const root = resolve(process.argv[2]);
const load = (path) => import(pathToFileURL(resolve(root, path)).href);
const Schema = await load('node_modules/effect/dist/Schema.js');
const orchestration = await load('packages/contracts/src/orchestrationV2.ts');
const environment = await load('packages/contracts/src/environment.ts');
const auth = await load('packages/contracts/src/auth.ts');
const schemas = {};
for (const method of ['subscribeShell', 'subscribeThread', 'dispatchCommand']) {
  for (const direction of ['input', 'output']) {
    schemas[`${method}.${direction}`] = orchestration.OrchestrationV2RpcSchemas[method][direction];
  }
}
schemas.ExecutionEnvironmentCapabilities = environment.ExecutionEnvironmentCapabilities;
schemas.ServerAuthDescriptor = auth.ServerAuthDescriptor;
schemas.AuthTokenExchangeRequest = auth.AuthTokenExchangeRequest;
schemas.AuthAccessTokenResult = auth.AuthAccessTokenResult;
function sorted(value) {
  if (Array.isArray(value)) return value.map(sorted);
  if (value && typeof value === 'object') return Object.fromEntries(
    Object.keys(value).sort().map(key => [key, sorted(value[key])]));
  return value;
}
const output = {};
for (const [name, schema] of Object.entries(schemas)) {
  if (!schema?.ast) throw new Error(`Missing upstream schema: ${name}`);
  output[name] = Schema.toJsonSchemaDocument(schema);
}
await writeFile(process.argv[3], JSON.stringify(sorted(output), null, 2) + '\n');
