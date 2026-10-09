const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

if (process.env.STATE_receipt) {
  const receipt = process.env.STATE_receipt;
  try {
    if (fs.existsSync(receipt) && !JSON.parse(fs.readFileSync(receipt, "utf8")).finished) {
      const result = spawnSync('python3', [path.join(process.env.GITHUB_WORKSPACE,
        'scripts/ci/github-cancel.py'), receipt], {
        env: { ...process.env, MIRROR_TOKEN: process.env.INPUT_TOKEN },
        stdio: 'inherit', timeout: 15000,
      });
      if (result.error || result.status !== 0) {
        console.log('::warning::Hosted cancellation cleanup was not confirmed');
      }
    }
  } finally {
    fs.rmSync(receipt, { force: true });
  }
} else {
  const receipt = path.join(process.env.RUNNER_TEMP, `hosted-${crypto.randomUUID()}.json`);
  fs.appendFileSync(process.env.GITHUB_STATE, `receipt=${receipt}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV, `TESSERA_CANCELLATION_RECEIPT=${receipt}\n`);
}
