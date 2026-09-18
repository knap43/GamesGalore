// Startup: the installed shelf has to be on screen from the local
// cache well before the (deliberately slow) catalog fetch resolves,
// and the reconciliation afterwards must not undo anything the user
// did in the meantime.
const { boot: bootApp, check, sleep, finish } = require('./harness');

const game = (id, title, platform) => ({
  id, title, platform, description: '', release_year: null,
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: 10 }],
  screenshots: [], cover: null, trailer: null,
});

// Two installed, three more that only the live catalog knows about.
const CACHED = [game('PC/Hollow Meridian', 'Hollow Meridian', 'PC'),
                game('Switch/Bramblewood', 'Bramblewood', 'Switch')];
const LIVE = [...CACHED.map(g => ({ ...g })),
              game('PC/Tidebreaker', 'Tidebreaker', 'PC'),
              game('PC/Static Choir', 'Static Choir', 'PC'),
              game('PS1/Nebula Drift', 'Nebula Drift', 'PS1')];

const INSTALLED = {
  'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' },
  'Switch/Bramblewood': { status: 'installed', local_dir: '/games/Switch/Bramblewood' },
};

async function boot({ cached = CACHED, libraryDelay = 600, libraryFails = false,
                      installStates = INSTALLED } = {}) {
  const calls = [];
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: true,
    emulators: {}, launch_overrides: {},
    save_sync: { enabled: true, device_name: 'test', switch_data_dir: null, title_ids: {} },
  };

  const invoke = async (cmd) => {
    calls.push(cmd);
    switch (cmd) {
      case 'get_settings': return settings;
      case 'get_cached_library': return cached;
      case 'fetch_library':
        await sleep(libraryDelay);
        if (libraryFails) throw new Error('connection refused');
        return LIVE.map(g => ({ ...g }));
      case 'get_install_states': return installStates;
      case 'detect_switch_data_dir': return null;
      case 'list_switch_title_ids': return [];
      default: return null;
    }
  };

  // settle: 0 on purpose — the whole point is to observe the app
  // during startup, not after it.
  const { win, doc, errors } = await bootApp({ invoke, settle: 0 });
  return { win, doc, calls, errors };
}

const titles = doc => [...doc.querySelectorAll('.card .title')].map(e => e.textContent.trim()).sort();
const footer = doc => doc.getElementById('sidebar-footer').textContent;

(async () => {
  // === the cached shelf paints long before the catalog lands =========
  {
    const { doc, calls, errors } = await boot();
    await sleep(150); // well inside the 600ms fetch

    check('installed titles are on screen before the fetch resolves',
          titles(doc), ['Bramblewood', 'Hollow Meridian']);
    check('the cache is read before the server is asked',
          calls.indexOf('get_cached_library') < calls.indexOf('fetch_library'), true);
    check('the Installed filter is on for the cached shelf',
          !!doc.querySelector('.nav-item[data-value="installed"].active'), true);
    check('the footer says the rest is still coming',
          footer(doc).includes('Loading the rest of your library'), true);
    check('the grid never showed the loading placeholder',
          doc.querySelector('.empty-state'), null);

    await sleep(700); // now let the live catalog arrive

    check('the full catalog replaces the cached shelf',
          doc.getElementById('total-count').textContent, '5');
    check('the filter still shows only what is installed',
          titles(doc), ['Bramblewood', 'Hollow Meridian']);
    check('the refresh hint is gone once the catalog is in',
          footer(doc).includes('Loading the rest of your library'), false);
    check('the footer counts the whole catalog', footer(doc).includes('2 of 5 titles'), true);
    check('no uncaught errors', errors, []);
  }

  // === an unreachable server leaves the shelf standing ===============
  {
    const { doc, errors } = await boot({ libraryDelay: 50, libraryFails: true });
    await sleep(500);

    check('installed games survive a failed fetch',
          titles(doc), ['Bramblewood', 'Hollow Meridian']);
    check('and are still counted as installed',
          footer(doc).includes('2 of 2 titles'), true);
    check('no uncaught errors on a failed fetch', errors, []);
  }

  // === no cache: the old behaviour, unchanged ========================
  {
    const { doc, errors } = await boot({ cached: [] });
    await sleep(150);

    const empty = doc.querySelector('.empty-state');
    check('with no cache the grid still reads as loading',
          empty && empty.textContent.includes('Loading your library'), true);
    check('and nothing claims to be refreshing behind it',
          footer(doc).includes('Loading the rest of your library'), false);

    await sleep(700);
    check('the catalog arrives as before', doc.getElementById('total-count').textContent, '5');
    check('and the installed default still applies',
          titles(doc), ['Bramblewood', 'Hollow Meridian']);
    check('no uncaught errors without a cache', errors, []);
  }

  // === a cache with nothing installed anymore ========================
  {
    // installs.json was cleared out from under the cache file. Nothing
    // is installed, so the filter must not switch itself on and leave
    // the user staring at an empty grid.
    const { doc, errors } = await boot({ installStates: {}, libraryDelay: 50 });
    await sleep(500);

    check('the filter stays off when nothing is installed',
          !!doc.querySelector('.nav-item[data-value="installed"].active'), false);
    check('so the whole catalog is showing', titles(doc).length, 5);
    check('no uncaught errors on a stale cache', errors, []);
  }

  // === the user's own filter choice outranks the startup default =====
  {
    const { doc, errors } = await boot();
    await sleep(150);

    doc.querySelector('.nav-item[data-value="installed"]').click(); // turn it off mid-refresh
    check('turning the filter off during the refresh takes effect',
          !!doc.querySelector('.nav-item[data-value="installed"].active'), false);

    await sleep(700);
    check('and the arriving catalog does not switch it back on',
          !!doc.querySelector('.nav-item[data-value="installed"].active'), false);
    check('so every title is on screen', titles(doc).length, 5);
    check('no uncaught errors after a mid-refresh toggle', errors, []);
  }

  finish();
})();
