// The published demo (docs/index.html) is the same app with no Tauri
// runtime behind it, kept in step by hand. This suite covers both
// halves of that: that it still works standing alone in a browser, and
// that it hasn't drifted structurally from the app it mirrors.
const fs = require('fs');
const { boot, check, sleep, finish, APP_HTML, DEMO_HTML } = require('./harness');

const idsIn = file => new Set(
  [...fs.readFileSync(file, 'utf8').matchAll(/\bid="([^"]+)"/g)].map(m => m[1])
);

(async () => {
  // === it stands alone ===============================================
  const { doc, win, errors } = await boot({ html: DEMO_HTML, settle: 500 });

  check('the demo runs with no Tauri runtime at all',
        win.__TAURI__ === undefined, true);
  check('the mock catalog is on screen', doc.querySelectorAll('.card').length > 0, true);
  check('and it opens on the installed shelf',
        !!doc.querySelector('.nav-item[data-value="installed"].active'), true);
  check('nothing claims to be refreshing a catalog it never fetches',
        doc.getElementById('sidebar-footer').textContent.includes('Loading the rest'), false);

  // A visitor's first click has to work, and the install button has to
  // do something visible rather than nothing.
  doc.querySelector('.card').click();
  await sleep(200);
  check('a card opens its detail view',
        doc.getElementById('view-detail').style.display !== 'none', true);
  check('the launch picker stays hidden with nothing to pick',
        doc.getElementById('launch-picker').style.display, 'none');

  // The mock catalog's two uninstalled titles are behind the Installed
  // filter the demo opens on, so clear it before looking for one.
  doc.getElementById('detail-back').click();
  await sleep(120);
  doc.querySelector('.nav-item[data-value="installed"]').click();
  await sleep(200);

  const uninstalled = [...doc.querySelectorAll('.card')]
    .find(c => c.textContent.includes('Ferrofluid'));
  check('an uninstalled title is reachable once the filter is off', !!uninstalled, true);
  uninstalled.click();
  await sleep(200);
  check('...and offers to install', doc.getElementById('cta-button').textContent.includes('Install'), true);

  doc.getElementById('cta-button').click();
  await sleep(500);
  check('the simulated install shows progress rather than completing instantly',
        doc.getElementById('install-progress').style.display !== 'none', true);

  check('no uncaught errors in the demo', errors, []);

  // === it hasn't drifted =============================================
  // Every element the app addresses by id must exist in the demo too.
  // This is the cheap half of keeping the two files in step, and it is
  // exactly the half that has actually broken before.
  const appIds = idsIn(APP_HTML);
  const demoIds = idsIn(DEMO_HTML);
  const missing = [...appIds].filter(id => !demoIds.has(id)).sort();
  check('the demo carries every id the app does', missing, []);

  finish();
})();
