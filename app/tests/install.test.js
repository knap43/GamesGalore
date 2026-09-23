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
const stats = doc => doc.getElementById('install-progress-stats').textContent;
// What someone reads across the row, in order.
const wholeLabel = doc => [label(doc), stats(doc)].filter(Boolean).join(' · ');
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
    check('...with no rate beside it, since nothing is transferring', stats(doc), '');
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
    status('PC/Tidebreaker', {
      status: 'downloading', file: 'game.bin', pct: 41,
      bytes_per_sec: 12_400_000, eta_secs: 214,
    });
    await sleep(60);

    check('a transferring title stops saying it is queued', cta(doc), 'Downloading…');
    check('...and names the file, the percentage, the rate and the wait',
          wholeLabel(doc), 'game.bin — 41% · 12.4 MB/s · 4 min left');
    check('...with the numbers kept apart from the filename, so a long path\n        gives way before they do',
          [label(doc), stats(doc)], ['game.bin — 41%', '12.4 MB/s · 4 min left']);
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

  // === what the speed reads like ======================================
  {
    const { doc, status } = await boot();
    await openGame(doc, 'Tidebreaker');
    doc.getElementById('cta-button').click();
    await sleep(60);

    const shows = async (payload) => {
      status('PC/Tidebreaker', { status: 'downloading', file: 'game.bin', pct: 50, ...payload });
      await sleep(60);
      return wholeLabel(doc);
    };

    check('a slow link is stated in kB/s',
          await shows({ bytes_per_sec: 240_000, eta_secs: 7200 }),
          'game.bin — 50% · 240 kB/s · 2h left');
    check('a fast one in MB/s, to one decimal',
          await shows({ bytes_per_sec: 118_000_000, eta_secs: 45 }),
          'game.bin — 50% · 118.0 MB/s · 45s left');
    check('the last seconds say so rather than counting down',
          await shows({ bytes_per_sec: 9_000_000, eta_secs: 3 }),
          'game.bin — 50% · 9.0 MB/s · almost done');
    // Before the first sample there is no rate and no estimate, and a
    // 0 B/s beside a moving bar is worse than saying nothing.
    check('nothing is invented before there is a measurement',
          await shows({ bytes_per_sec: 0, eta_secs: null }),
          'game.bin — 50%');
    check('an hour and a bit is said as such',
          await shows({ bytes_per_sec: 3_000_000, eta_secs: 4500 }),
          'game.bin — 50% · 3.0 MB/s · 1h 15m left');
  }

  // === games already on disk ========================================
  // installs.json is this app's memory; the install directories are
  // the fact. A game copied in by hand, or sitting on a drive that has
  // just been added, is installed whether or not it was installed from
  // here — and has to be there under the Installed filter, which is
  // the shelf people play from.
  {
    const rt = createRuntime();
    let reconciled = false;
    const invoke = async (cmd) => {
      switch (cmd) {
        case 'get_settings': return {
          server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
          install_roots: ['/games'], emulators: {}, launch_overrides: {}, prefix_root: '',
          save_sync: { enabled: false, device_name: 'test', switch_data_dir: null, title_ids: {} },
        };
        case 'get_cached_library': return [];
        case 'fetch_library': return LIBRARY.map(g => ({ ...g }));
        // The backend adopts what it finds on disk, so the records the
        // frontend then reads already include it.
        case 'reconcile_installs': reconciled = true; return ['PC/Tidebreaker'];
        case 'get_install_states':
          return reconciled
            ? { 'PC/Tidebreaker': { status: 'installed', local_dir: '/games/PC/Tidebreaker' } }
            : {};
        default: return null;
      }
    };

    const { doc, errors } = await bootApp({ invoke, listen: rt.listen });
    check('the disk is consulted before the records are read', reconciled, true);

    const installedNav = doc.querySelector('.nav-item[data-type="status"][data-value="installed"]');
    check('a game found on disk counts as installed',
          installedNav.querySelector('.count').textContent, '1');
    check('...and the filter it turned on is showing',
          installedNav.classList.contains('active'), true);
    check('...with that game on the shelf',
          Array.from(doc.querySelectorAll('.card')).map(c => c.dataset.id),
          ['PC/Tidebreaker']);
    check('no uncaught errors on the adoption path', errors, []);
  }

  // === the shelf while a game is arriving ===========================
  // The Installed filter is the shelf people play from, and it is on
  // by default once anything is installed. A title whose download has
  // just started belongs on it: filtering by "installed" alone took
  // the game off screen at the moment someone pressed Install, taking
  // its progress with it.
  {
    const rt = createRuntime();
    const status = (id, payload) => rt.emit('install:status', [id, payload]);
    const invoke = async (cmd) => {
      switch (cmd) {
        case 'get_settings': return {
          server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
          install_roots: ['/games'], emulators: {}, launch_overrides: {}, prefix_root: '',
          save_sync: { enabled: false, device_name: 'test', switch_data_dir: null, title_ids: {} },
        };
        case 'get_cached_library': return [];
        case 'fetch_library': return LIBRARY.map(g => ({ ...g }));
        case 'get_install_states':
          return { 'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' } };
        default: return null;
      }
    };

    const { doc, errors } = await bootApp({ invoke, listen: rt.listen });
    const shelf = () => Array.from(doc.querySelectorAll('.card')).map(c => c.dataset.id);
    const sub = id => doc.querySelector(`.card[data-id="${id}"] .sub`).textContent;

    check('the filter is on, with the installed title on the shelf',
          shelf(), ['PC/Hollow Meridian']);

    status('PC/Tidebreaker', { status: 'queued', file: '', pct: 0 });
    await sleep(60);
    check('a queued title joins the shelf rather than vanishing from it',
          shelf().includes('PC/Tidebreaker'), true);
    check('...saying what it is waiting for', sub('PC/Tidebreaker'), 'Queued');

    status('PC/Tidebreaker', { status: 'downloading', file: 'game.bin', pct: 37, bytes_per_sec: 0, eta_secs: null });
    await sleep(60);
    check('...and stays while it downloads', shelf().includes('PC/Tidebreaker'), true);
    check('...with its progress on the card', sub('PC/Tidebreaker'), 'Installing… 37%');

    status('PC/Tidebreaker', { status: 'failed', message: 'not enough room' });
    await sleep(60);
    check('a failure stays put, since vanishing hides it',
          shelf().includes('PC/Tidebreaker'), true);
    check('...and says so', sub('PC/Tidebreaker'), 'Install failed');

    status('PC/Tidebreaker', { status: 'installed', local_dir: '/games/PC/Tidebreaker' });
    await sleep(60);
    check('once installed it is simply one of the shelf',
          shelf().sort(), ['PC/Hollow Meridian', 'PC/Tidebreaker']);
    check('...and its card goes back to describing the game',
          sub('PC/Tidebreaker'), '2020');

    status('PC/Tidebreaker', { status: 'not_installed' });
    await sleep(60);
    check('a title that is none of those is off the shelf again',
          shelf(), ['PC/Hollow Meridian']);
    check('no uncaught errors while the shelf changed under it', errors, []);
  }

  // === opening a game's folder =====================================
  // The files are the point of the app having installed anything, and
  // reaching them meant remembering which drive a game went to.
  {
    const rt = createRuntime();
    const opened = [];
    let refuse = false;
    const invoke = async (cmd, args) => {
      switch (cmd) {
        case 'get_settings': return {
          server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
          install_roots: ['/games'], emulators: {}, launch_overrides: {}, prefix_root: '',
          save_sync: { enabled: false, device_name: 'test', switch_data_dir: null, title_ids: {} },
        };
        case 'get_cached_library': return [];
        case 'fetch_library': return LIBRARY.map(g => ({ ...g }));
        case 'get_install_states':
          return { 'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' } };
        case 'list_launch_candidates': return [];
        case 'open_install_dir':
          if (refuse) throw 'PC/Hollow Meridian is not on this machine any more';
          opened.push(args.gameId);
          return null;
        default: return null;
      }
    };

    const { doc, errors } = await bootApp({ invoke, listen: rt.listen });
    const button = () => doc.getElementById('open-folder-button');

    // The installed title turned the filter on, so the uninstalled one
    // needs it off again to be reachable.
    doc.querySelector('.nav-item[data-type="status"][data-value="installed"]').click();
    await sleep(60);
    await openGame(doc, 'Tidebreaker');
    check('a game that is not installed has no folder to open',
          shown(doc, 'open-folder-button'), false);

    doc.getElementById('detail-back').click();
    await sleep(80);
    await openGame(doc, 'Hollow Meridian');
    check('an installed one does', shown(doc, 'open-folder-button'), true);

    button().click();
    await sleep(60);
    check('the button asks the backend by id, not by path',
          opened, ['PC/Hollow Meridian']);

    // The backend resolves the directory, so it is also the one that
    // knows when there is no longer a directory to resolve.
    refuse = true;
    button().click();
    await sleep(60);
    check('a folder that has gone says so on the page',
          doc.getElementById('install-error').textContent,
          'PC/Hollow Meridian is not on this machine any more');
    check('...as a problem rather than as good news',
          doc.getElementById('install-error').classList.contains('is-good'), false);
    check('no uncaught errors from the folder button', errors, []);
  }

  finish();
})();
