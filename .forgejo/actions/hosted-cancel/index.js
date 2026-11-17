'use strict';
// #844. When a Forgejo job is cancelled or superseded, the runner kills the
// bridge step and skips ordinary always() steps, but still runs action post
// steps. Main phase: register a receipt path for the bridge. Post phase: hand
// the receipt to scripts/ci/github-cancel.py, which cancels only the GitHub run
// that this job's bridge dispatched, and only if the bridge never finished.
// The hook never fails the job: every problem is a warning.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

const receipt = process.env.STATE_receipt;
if (!receipt) {
  const created = path.join(process.env.RUNNER_TEMP, `okilum-hosted-${crypto.randomUUID()}.json`);
  fs.appendFileSync(process.env.GITHUB_STATE, `receipt=${created}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV, `OKILUM_HOSTED_RECEIPT=${created}\n`);
  console.log('Registered owned hosted run cancellation');
} else {
  try {
    if (!fs.existsSync(receipt)) {
      console.log('No hosted dispatch recorded; nothing to cancel');
    } else {
      const script = path.resolve(__dirname, '../../../scripts/ci/github-cancel.py');
      const result = spawnSync('python3', ['-I', script, receipt],
        { stdio: 'inherit', timeout: 120000, env: process.env });
      if (result.error || result.status !== 0) {
        console.log('::warning::Owned hosted run cancellation did not complete');
      }
    }
  } catch {
    console.log('::warning::Owned hosted run cancellation receipt could not be processed');
  } finally {
    for (const file of [receipt, `${receipt}.tmp`]) {
      try { fs.rmSync(file, { force: true }); } catch {}
    }
  }
}
