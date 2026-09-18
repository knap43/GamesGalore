// Runs every *.test.js in this directory, one process each so a crash
// in one suite can't take the others' results with it.
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const suites = fs.readdirSync(__dirname).filter(f => f.endsWith('.test.js')).sort();
const failed = [];

for (const suite of suites) {
  console.log(`\n${'='.repeat(64)}\n${suite}\n${'='.repeat(64)}`);
  const run = spawnSync(process.execPath, [path.join(__dirname, suite)], { stdio: 'inherit' });
  if (run.status !== 0) failed.push(suite);
}

console.log(`\n${'='.repeat(64)}`);
if (failed.length) {
  console.log(`FAILED: ${failed.join(', ')}`);
  process.exit(1);
}
console.log(`all ${suites.length} suites passed`);
