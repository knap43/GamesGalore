// The launch picker: when a title offers a choice of executables, what
// order it offers them in, what it passes to launch_game, and how it
// sits in the keyboard navigation chain.
const { boot: bootApp, check, sleep, finish, createRuntime } = require('./harness');

function mockGames() {
  return [
    { id: 'PC/Hollow Meridian', title: 'Hollow Meridian', platform: 'PC',
      description: 'x', files: [{ filename: 'bin/HollowMeridian.exe', size_bytes: 40000 }],
      screenshots: [], cover: null, trailer: null },
    { id: 'PC/Ferrofluid', title: 'Ferrofluid', platform: 'PC',
      description: 'y', files: [{ filename: 'Ferrofluid.exe', size_bytes: 1000 }],
      screenshots: [], cover: null, trailer: null },
  ];
}

async function boot(candidatesFor) {
  const calls = [];
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: true,
    emulators: { PS1: { command: 'duckstation-qt', args_prefix: [], version_flag: '-version' },
                 PS2: { command: 'pcsx2-qt', args_prefix: [], version_flag: '--version' },
                 PC:  { command: 'wine', args_prefix: [], version_flag: '--version' },
                 Switch: { command: 'eden', args_prefix: [], version_flag: '--version' } },
    launch_overrides: {},
  };

  const rt = createRuntime();
  const emit = (id, status) => rt.emit('install:status', [id, status]);

  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'fetch_library': return mockGames();
      case 'get_cached_library': return [];
      case 'get_install_states': return {
        'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' },
        'PC/Ferrofluid': { status: 'installed', local_dir: '/games/PC/Ferrofluid' },
      };
      case 'list_launch_candidates': return candidatesFor(args.installDir);
      case 'launch_game': return null;
      case 'uninstall_game':
        emit(args.gameId, { status: 'not_installed' });
        return null;
      default: return null;
    }
  };

  const { win, doc, errors } = await bootApp({ invoke, listen: rt.listen });
  return { win, doc, calls, settings, errors };
}

(async () => {
  // === three executables: the picker appears ===========================
  const three = ['bin/HollowMeridian.exe', 'unins000.exe', 'redist/vcredist_x64.exe'];
  let { win, doc, calls, settings, errors } = await boot(dir =>
    dir.endsWith('Hollow Meridian') ? three : ['Ferrofluid.exe']);

  check('no uncaught errors on boot', errors, []);

  const cards = [...doc.querySelectorAll('.card')];
  check('both mock games rendered', cards.length, 2);

  const hmCard = cards.find(c => c.textContent.includes('Hollow Meridian'));
  hmCard.click();
  await sleep(150);

  const picker = doc.getElementById('launch-picker');
  const trigger = doc.getElementById('launch-trigger');
  const menu = doc.getElementById('launch-menu');
  check('picker visible for a 3-executable title', picker.style.display, 'inline-flex');
  check('menu starts closed', menu.hidden, true);
  check('trigger shows the best candidate, filename only',
        doc.getElementById('launch-current').textContent, 'HollowMeridian.exe');
  check('trigger carries the full path as a tooltip', trigger.title, three[0]);
  check('no native select anywhere', doc.querySelectorAll('select').length, 0);

  trigger.click();
  await sleep(50);
  check('clicking the trigger opens the menu', menu.hidden, false);
  check('trigger reports expanded', trigger.getAttribute('aria-expanded'), 'true');
  const opts = [...menu.querySelectorAll('.launch-option')];
  check('every candidate is offered, best first',
        opts.map(o => o.dataset.launchName), three);
  check('the current choice is marked selected',
        opts.findIndex(o => o.classList.contains('selected')), 0);
  check('directory prefix is split out for a nested path',
        opts[0].querySelector('.launch-option-dir').textContent, 'bin/');
  check('highlight starts on the current choice',
        opts.findIndex(o => o.classList.contains('highlighted')), 0);
  check('no override recorded merely by opening', settings.launch_overrides, {});

  // Escape must close the menu, not the whole detail view.
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await sleep(30);
  check('Escape closes the menu', menu.hidden, true);
  check('Escape did not also leave the detail view',
        doc.getElementById('view-detail').style.display, 'block');
  check('focus returns to the trigger', doc.activeElement.id, 'launch-trigger');

  // Play with no override -> backend does its own ranking.
  doc.getElementById('cta-button').click();
  await sleep(50);
  let launch = calls.filter(c => c[0] === 'launch_game').pop();
  check('launch passes executable:null when nothing was chosen',
        launch[1].executable, null);

  // === choosing with the keyboard ======================================
  trigger.click();
  await sleep(40);
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  await sleep(30);
  check('arrows move the highlight, not page focus',
        [...menu.querySelectorAll('.launch-option')].findIndex(o => o.classList.contains('highlighted')), 2);
  check('focus stayed on the trigger while arrowing', doc.activeElement.id, 'launch-trigger');
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  await sleep(80);
  check('Enter commits the highlighted row', settings.launch_overrides,
        { 'PC/Hollow Meridian': three[2] });
  check('menu closed after committing', menu.hidden, true);
  check('trigger now shows the chosen file',
        doc.getElementById('launch-current').textContent, 'vcredist_x64.exe');

  doc.getElementById('cta-button').click();
  await sleep(50);
  launch = calls.filter(c => c[0] === 'launch_game').pop();
  check('launch passes the chosen executable', launch[1].executable, three[2]);
  check('launch still passes install dir and platform',
        [launch[1].installDir, launch[1].platform],
        ['/games/PC/Hollow Meridian', 'PC']);

  // Highlight wraps rather than sticking at an edge.
  trigger.click();
  await sleep(40);
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  await sleep(20);
  check('highlight wraps past the end',
        [...menu.querySelectorAll('.launch-option')].findIndex(o => o.classList.contains('highlighted')), 0);

  // Choosing the default again clears the override.
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  await sleep(80);
  check('reselecting the default clears the override', settings.launch_overrides, {});

  // Clicking outside closes without choosing.
  trigger.click();
  await sleep(40);
  doc.getElementById('description').click();
  await sleep(30);
  check('a click outside closes the menu', menu.hidden, true);

  // === navigation chain ================================================
  doc.getElementById('cta-button').focus();
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  check('right from Play reaches the picker, which now sits next to it',
        doc.activeElement.id, 'launch-trigger');
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  check('right again reaches Uninstall', doc.activeElement.id, 'uninstall-button');
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
  check('left from Uninstall goes back to the picker',
        doc.activeElement.id, 'launch-trigger');
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
  check('...and left again to Play', doc.activeElement.id, 'cta-button');

  // The picker sits between them in the document too, not just visually.
  const order = [...doc.querySelectorAll('.cta-row > *')].map(el => el.id);
  check('the row reads Play, picker, Uninstall',
        order.slice(0, 3), ['cta-button', 'launch-picker', 'uninstall-button']);

  // === uninstall hides the picker ======================================
  doc.getElementById('uninstall-button').click();
  await sleep(120);
  check('picker hidden once the title is uninstalled',
        doc.getElementById('launch-picker').style.display, 'none');

  check('still no uncaught errors', errors, []);

  // === a single executable offers no choice ============================
  doc.getElementById('detail-back').click();
  await sleep(80);
  const ffCard = [...doc.querySelectorAll('.card')].find(c => c.textContent.includes('Ferrofluid'));
  ffCard.click();
  await sleep(150);
  check('picker hidden for a single-executable title',
        doc.getElementById('launch-picker').style.display, 'none');
  doc.getElementById('cta-button').focus();
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  check('right from Play skips the hidden picker and lands on Uninstall',
        doc.activeElement.id, 'uninstall-button');
  win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
  check('left from Uninstall skips it too, straight back to Play',
        doc.activeElement.id, 'cta-button');

  // === backend failure degrades quietly ================================
  const boom = await boot(() => { throw new Error('install dir vanished'); });
  await sleep(100);
  const c2 = [...boom.doc.querySelectorAll('.card')].find(c => c.textContent.includes('Hollow Meridian'));
  c2.click();
  await sleep(150);
  check('a failing candidate lookup leaves the picker hidden',
        boom.doc.getElementById('launch-picker').style.display, 'none');
  boom.doc.getElementById('cta-button').click();
  await sleep(50);
  check('Play still works when candidates could not be listed',
        boom.calls.some(c => c[0] === 'launch_game'), true);
  check('no uncaught errors from the failure path', boom.errors, []);

  finish();
})();
