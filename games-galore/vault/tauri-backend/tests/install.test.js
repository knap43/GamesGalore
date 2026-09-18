// The install lifecycle as the user sees it: a title waiting its turn
// behind another says so, a downloading one reports progress against
// the whole title, cancelling works from either state, and a failure
// (no room on the disk, a truncated transfer) is put on screen rather
// than swallowed.
const { boot: bootApp, check, sleep, finish, createRuntime, openGame } = require('./harness');

const game = (id, title, platform, size) => ({
  id, title, platform, release_year: 2020, description: 'A game.',
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: size }],
  screenshots: [], cover: null, trailer: null,
});

const LIBRARY = [
  game('PC/Hollow Meridian', 'Hollow Meridian', 'PC', 4e9),
  game('PC/Tidebreaker', 'Tidebreaker', 'PC', 9e9),
];

async function boot() {
  const calls = [];
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: {}, launch_overrides: {}, prefix_root: '',
    save_sync: { enabled: false, device_name: 'test', switch_data_dir: null, title_ids: {} },
  };
  const rt = createRuntime();
  const status = (id, payload) => rt.emit('install:status', [id, payload]);

  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    switch (cmd) {
      case 'get_settings': return settings;
      case 'get_cached_library': return [];
      case 'fetch_library': return LIBRARY.map(g => ({ ...g }));
      case 'get_install_states': return {};
      case 'list_launch_candidates': return [];
      // The real backend queues, then downloads, then finishes — all
      // through this event, never through the promise this returns.
      case 'install_game': return null;
      case 'cancel_install':
        status(args.gameId, { status: 'not_installed' });
        return null;
      default: return null;
    }
  };

  const { win, doc, errors } = await bootApp({ invoke, listen: rt.listen });
  return { win, doc, calls, errors, status };
}

const cta = doc => doc.getElementById('cta-button').textContent.trim();
const label = doc => doc.getElementById('install-progress-label').textContent;
const shown = (doc, id) => doc.getElementById(id).style.display !== 'none';

(async () => {
  // === a title waiting its turn =====================================
  {
    const { doc, calls, errors, status } = await boot();
    await openGame(doc, 'Hollow Meridian');
    check('an uninstalled title offers to install', cta(doc), 'Install');

    doc.getElementById('cta-button').click();
    await sleep(80);
    check('the install was asked for',
          calls.some(c => c[0] === 'install_game'), true);

    status('PC/Hollow Meridian', { status: 'queued' });
    await sleep(80);
    check('a queued title says it is queued', cta(doc), 'Queued');
    check('...and says what it is waiting for',
          label(doc), 'Waiting for another install to finish');
    check('...offering no misleading percentage',
          doc.getElementById('install-progress-fill').style.width, '0%');
    check('...and can still be cancelled',
          shown(doc, 'cancel-install-button'), true);

    // Cancelling from the queue is free on the backend — nothing has
    // been requested or written — and has to work from the UI too.
    doc.getElementById('cancel-install-button').click();
    await sleep(120);
    check('cancelling a queued install is possible',
          calls.some(c => c[0] === 'cancel_install'), true);
    check('...and puts the title back to Install', cta(doc), 'Install');
    check('no uncaught errors around the queue', errors, []);
  }

  // === and then transferring ========================================
  {
    const { doc, errors, status } = await boot();
    await openGame(doc, 'Tidebreaker');
    doc.getElementById('cta-button').click();
    await sleep(60);

    status('PC/Tidebreaker', { status: 'queued' });
    await sleep(60);
    status('PC/Tidebreaker', { status: 'downloading', file: 'game.bin', pct: 41 });
    await sleep(60);

    check('a transferring title stops saying it is queued', cta(doc), 'Downloading…');
    check('...and names the file and the percentage', label(doc), 'game.bin — 41%');
    check('...against the whole title, not the file',
          doc.getElementById('install-progress-fill').style.width, '41%');

    status('PC/Tidebreaker', { status: 'installed', local_dir: '/games/PC/Tidebreaker' });
    await sleep(80);
    check('a finished install offers to play', cta(doc), 'Play');
    check('...and to uninstall', shown(doc, 'uninstall-button'), true);
    check('...with the progress bar gone', shown(doc, 'install-progress'), false);
    check('no uncaught errors through the lifecycle', errors, []);
  }

  // === a refusal is put on screen ===================================
  {
    const { doc, errors, status } = await boot();
    await openGame(doc, 'Hollow Meridian');
    doc.getElementById('cta-button').click();
    await sleep(60);

    // What the backend sends when the disk hasn't room for the title.
    status('PC/Hollow Meridian', {
      status: 'failed',
      message: 'not enough room in /games/PC/Hollow Meridian: this needs about 8.2 GB, and 1.1 GB is free',
    });
    await sleep(80);

    const err = doc.getElementById('install-error');
    check('a refused install is visible', err.style.display, 'block');
    check('...and says how much room was wanted',
          err.textContent.includes('8.2 GB'), true);
    check('...and how much there was', err.textContent.includes('1.1 GB is free'), true);
    check('...in red rather than the save notice green',
          err.classList.contains('is-good'), false);
    check('...and the title can be tried again', cta(doc), 'Install');

    // A truncated transfer reports the same way.
    status('PC/Hollow Meridian', {
      status: 'failed',
      message: 'game.bin arrived incomplete: expected 4000000000 bytes, got 1200000000',
    });
    await sleep(80);
    check('a truncated transfer is reported too',
          doc.getElementById('install-error').textContent.includes('arrived incomplete'), true);
    check('no uncaught errors around a failure', errors, []);
  }

  finish();
})();
