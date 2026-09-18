/*
 * Generates docs/index.html — the GitHub Pages demo — from the app's
 * own frontend.
 *
 *   node tools/build-demo.js           write docs/index.html
 *   node tools/build-demo.js --check   fail if it is out of date
 *
 * The demo was kept in step by hand for months, which worked exactly
 * as well as that ever does: the two files drifted, a duplicated
 * function and a mangled comment made it into the published copy, and
 * every frontend change carried a silent obligation nobody could see.
 * The demo is now a build artifact — edit the app, run this, commit
 * both — and CI runs --check so drift fails the build instead of the
 * demo.
 *
 * Each patch below is applied exactly once and asserts that it
 * matched. If a patch stops matching because the app moved on, this
 * refuses to write a half-transformed file and says which one, which
 * is the entire point: the failure lands on the person changing the
 * code rather than on a visitor a week later.
 */

const fs = require('fs');
const path = require('path');

const APP = path.join(__dirname, '..', 'frontend', 'index.html');
const DEMO = path.join(__dirname, '..', '..', 'docs', 'index.html');

/*
 * Everything the demo does differently, and why:
 *
 * - It says out loud that it is a demo, since a visitor has no other
 *   way to know the catalog is invented.
 * - Installing plays out a realistic progress sequence instead of
 *   completing instantly. The app's own browser-preview fallback flips
 *   the game to "installed" immediately, which skips straight past the
 *   progress bar and the cancel button — both real, deliberate parts
 *   of the UI, and both worth showing.
 * - Play says what it would have done. A silent no-op reads as a
 *   broken button to someone with no reason to expect otherwise.
 *
 * Everything else about running without a backend comes from the app
 * itself: every Tauri call is already guarded behind
 * `const TAURI = window.__TAURI__ || null`, so opening the file
 * directly takes each preview branch and falls back to the mock
 * catalog.
 */
const PATCHES = [
  {
    name: 'demo banner styles',
    find: '  .topbar {\n',
    replace: `  .demo-banner {
    background: linear-gradient(135deg, rgba(255,79,158,0.14), rgba(155,92,246,0.14));
    border: 1px solid rgba(255,255,255,0.12);
    border-radius: var(--radius-pill);
    padding: 10px 18px;
    font-size: 12.5px;
    color: var(--text-muted);
    margin-bottom: 20px;
  }

  .topbar {
`,
  },
  {
    name: 'demo banner markup',
    find: '      <div class="topbar">\n',
    replace: `      <div class="demo-banner">
        This is a UI demo — installs and playtime are simulated, and no real files are downloaded or launched.
      </div>

      <div class="topbar">
`,
  },
  {
    name: 'data layer comment',
    find: ` * in a browser. Everything below (render, filter, sort) is written
 * against the shape of that array alone, so neither path needs any
 * special handling further down.`,
    replace: ` * in a browser, which is what this hosted demo build always does.
 * Everything below (render, filter, sort) is written against the
 * shape of that array alone, so neither path needs any special
 * handling further down.`,
  },
  {
    name: 'simulated install',
    find: `    // Browser-preview fallback: no server to actually install from,
    // so simulate completion instantly rather than doing nothing.
    game.installed = true;
    refreshInstallDependentUI(game.id);`,
    replace: `    // Demo build: plays out a realistic progress sequence rather than
    // completing instantly, since an instant completion would skip
    // right past the progress bar and cancel button — both real,
    // deliberate pieces of this app.
    simulateDemoInstall(game);`,
  },
  {
    name: 'simulated install implementation',
    find: 'function cancelInstall(game) {',
    replace: `function simulateDemoInstall(game) {
  const files = (game.files && game.files.length) ? game.files : [{ filename: \`\${game.title}.bin\` }];
  let fileIdx = 0;
  let pct = 0;

  applyInstallStatus(game, {
    status: 'downloading', file: files[0].filename, pct: 0,
    bytes_per_sec: 0, eta_secs: null,
  });
  refreshInstallDependentUI(game.id);

  const tick = () => {
    // If the visitor cancelled (or uninstalled) mid-simulation,
    // installProgress will already be cleared — stop rather than fight
    // that a moment later.
    if (!game.installProgress) return;

    pct += 8 + Math.random() * 12;
    if (pct >= 100) {
      fileIdx++;
      if (fileIdx >= files.length) {
        applyInstallStatus(game, { status: 'installed', local_dir: \`/demo/\${game.platform}/\${game.title}\` });
        refreshInstallDependentUI(game.id);
        return;
      }
      pct = 0;
    }
    applyInstallStatus(game, {
      status: 'downloading',
      file: files[fileIdx].filename,
      pct: Math.round(pct),
      // Invented, like everything else in the demo, but invented in
      // the shape the real thing reports: a rate that wobbles and an
      // estimate derived from it.
      bytes_per_sec: 40e6 + Math.random() * 25e6,
      eta_secs: Math.round((100 - pct) * 1.4) + 3,
    });
    refreshInstallDependentUI(game.id);
    setTimeout(tick, 180 + Math.random() * 220);
  };

  setTimeout(tick, 180 + Math.random() * 220);
}

function cancelInstall(game) {`,
  },
  {
    name: 'play button message',
    find: `async function launchGame(game) {
  if (!TAURI) return; // nothing to actually launch in a browser preview`,
    replace: `async function launchGame(game) {
  if (!TAURI) {
    // Demo build: nothing can actually launch here, but a silent no-op
    // reads as a broken button to a visitor who doesn't know that's
    // expected. Said on the button itself rather than in a blocking
    // alert() dialog.
    const btn = document.getElementById('cta-button');
    if (btn) {
      const original = btn.textContent;
      btn.textContent = 'Would launch via your emulator ↗';
      setTimeout(() => { btn.textContent = original; }, 1600);
    }
    return;
  }`,
  },
];

function build() {
  let out = fs.readFileSync(APP, 'utf8');
  for (const patch of PATCHES) {
    const occurrences = out.split(patch.find).length - 1;
    if (occurrences !== 1) {
      throw new Error(
        `the "${patch.name}" patch matched ${occurrences} times, expected exactly 1 — ` +
        'the app\'s frontend has moved and tools/build-demo.js needs updating to match'
      );
    }
    out = out.replace(patch.find, patch.replace);
  }
  return out;
}

const check = process.argv.includes('--check');
let generated;
try {
  generated = build();
} catch (err) {
  console.error(`build-demo: ${err.message}`);
  process.exit(1);
}

if (check) {
  const current = fs.existsSync(DEMO) ? fs.readFileSync(DEMO, 'utf8') : '';
  if (current !== generated) {
    console.error(
      'build-demo: docs/index.html is out of date with app/frontend/index.html.\n' +
      '            Run `npm run build:demo` from app/tests and commit the result.'
    );
    process.exit(1);
  }
  console.log('build-demo: docs/index.html is up to date');
} else {
  fs.writeFileSync(DEMO, generated);
  console.log(`build-demo: wrote ${DEMO}`);
}
