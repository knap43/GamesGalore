// The "Fetch details" button: when it is offered, what it asks the
// server for, and what the view does with the answer.
const { boot: bootApp, check, sleep, finish, createRuntime, openGame } = require('./harness');

const game = (id, title, extra = {}) => ({
  id, title, platform: 'PC', release_year: null, description: '',
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: 10 }],
  screenshots: [], cover: null, trailer: null, genre: null, tags: [], players: null,
  ...extra,
});

// Before: no description, no cover. After a fetch: both.
const BARE = [
  game('PC/Bare Title', 'Bare Title'),
  game('PC/Complete Title', 'Complete Title', {
    description: 'Already written up.', cover: 'http://x/cover.jpg',
    screenshots: ['http://x/cover.jpg'], release_year: 2019,
  }),
];
const FILLED = [
  game('PC/Bare Title', 'Bare Title', {
    description: 'A long dark corridor of a game.', cover: 'http://x/cover.jpg',
    screenshots: ['http://x/cover.jpg'], release_year: 2021, genre: 'RPG',
  }),
  BARE[1],
];

async function boot({ result, fails = false, delay = 0 } = {}) {
  const calls = [];
  let fetched = false;
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: {}, launch_overrides: {}, prefix_root: '', sort: 'alpha',
    save_sync: { enabled: false, device_name: 'test', switch_data_dir: '', title_ids: {}, token: '' },
  };
  const rt = createRuntime();
  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'get_cached_library': return [];
      // The catalog changes underneath, exactly as it does when the
      // server writes the files and the client refetches.
      case 'fetch_library': return (fetched ? FILLED : BARE).map(g => ({ ...g }));
      case 'get_install_states': return {};
      case 'get_playtime': return {};
      case 'list_launch_candidates': return [];
      case 'fetch_metadata':
        if (delay) await sleep(delay);
        if (fails) throw new Error('server returned 502: RAWG rejected the API key');
        fetched = true;
        return result || {
          title: 'Bare Title', matched: 'Bare Title', error: null, skipped: [],
          wrote: ['README.md', 'cover.jpg', 'screenshot-01.jpg'],
        };
      default: return null;
    }
  };
  const t = await bootApp({ invoke, listen: rt.listen });
  return { ...t, calls, settings };
}

const button = doc => doc.getElementById('fetch-metadata');
const shown = el => el.style.display !== 'none';

const openSettings = async (doc) => {
  doc.querySelector('.nav-item[data-type="settings"]').click();
  await sleep(150);
};

(async () => {
  // A slow-ish server, so the in-flight state is observable — fetching
  // a cover and half a dozen screenshots is not instant in life either.
  const { doc, calls, errors } = await boot({ delay: 300 });

  await openGame(doc, 'Complete Title');
  check('a game that already has both is not offered a fetch',
        shown(button(doc)), false);

  doc.getElementById('detail-back').click();
  await sleep(150);
  await openGame(doc, 'Bare Title');
  check('a game missing its details is', shown(button(doc)), true);
  check('...and the description falls back to the placeholder meanwhile',
        doc.getElementById('description').textContent.length > 0, true);

  button(doc).click();
  await sleep(80); // mid-flight
  check('the button says what it is doing', button(doc).textContent, 'Fetching…');
  check('...and cannot be pressed twice', button(doc).disabled, true);

  await sleep(600);
  const asked = calls.find(c => c[0] === 'fetch_metadata');
  check('the server was asked for this game',
        [asked[1].gameId, asked[1].serverBase], ['PC/Bare Title', 'http://x:8420']);

  check('the description that arrived is on screen',
        doc.getElementById('description').textContent.includes('long dark corridor'), true);
  check('...the year with it',
        doc.getElementById('detail-meta').textContent.includes('2021'), true);
  check('...and the genre from the sidecar',
        doc.getElementById('detail-meta').textContent.includes('RPG'), true);
  check('the button retires once there is nothing left to fetch',
        shown(button(doc)), false);
  check('what it did is reported',
        doc.getElementById('install-error').textContent.includes('Added 3 files'), true);
  check('no uncaught errors', errors, []);

  // The sidebar's genre section appears because the fetch supplied one.
  check('a genre fetched for one game populates the filter',
        [...doc.querySelectorAll('#genre-nav .nav-item')].map(e => e.dataset.value), ['RPG']);

  // A server that can't answer says so, and the button comes back.
  {
    const t = await boot({ fails: true });
    await openGame(t.doc, 'Bare Title');
    button(t.doc).click();
    await sleep(300);
    const note = t.doc.getElementById('install-error');
    check('a failure is reported', note.style.display, 'block');
    check('...with the reason', note.textContent.includes('rejected the API key'), true);
    check('...in red', note.classList.contains('is-good'), false);
    check('...and the button is offered again',
          [shown(button(t.doc)), button(t.doc).disabled], [true, false]);
    check('no uncaught errors on the failure path', t.errors, []);
  }

  // RAWG matching the wrong game is worth saying out loud.
  {
    const t = await boot({ result: {
      title: 'Bare Title', matched: 'Bare Title: Director\'s Cut', error: null,
      wrote: ['README.md'], skipped: [],
    } });
    await openGame(t.doc, 'Bare Title');
    button(t.doc).click();
    await sleep(400);
    check('a different name on RAWG is named in the note',
          t.doc.getElementById('install-error').textContent.includes("Director's Cut"), true);
  }

  // === the settings block =============================================
  {
    const t = await boot();
    await openSettings(t.doc);

    const key = t.doc.getElementById('rawg-key-input');
    check('the key field is on the settings screen', !!key, true);
    check('...and is not shown in the clear', key.type, 'password');

    key.value = 'a-rawg-key';
    key.dispatchEvent(new t.win.Event('change'));
    await sleep(150);
    check('...and is persisted with everything else',
          t.settings.rawg_key, 'a-rawg-key');
  }

  // === the library-wide run ===========================================
  {
    // Slow enough to observe mid-run, as a real fetch of a cover and
    // half a dozen screenshots is.
    const t = await boot({ delay: 400 });
    await openSettings(t.doc);

    t.doc.getElementById('fetch-all-metadata').click();
    await sleep(120);
    const status = t.doc.getElementById('metadata-status');
    check('the run reports which game it is on',
          /Fetching 1 of 1: Bare Title/.test(status.textContent), true);
    check('...and the button cannot be pressed again mid-run',
          t.doc.getElementById('fetch-all-metadata').disabled, true);

    await sleep(800);
    check('...then says what it managed',
          status.textContent, 'Filled in 1 game.');
    check('...and the button comes back',
          t.doc.getElementById('fetch-all-metadata').disabled, false);

    const bulk = t.calls.filter(c => c[0] === 'fetch_metadata');
    check('only the incomplete game was looked up', bulk.length, 1);
    check('...and the server was told to skip its rescan until the end',
          bulk[0][1].bulk, true);
    check('no uncaught errors during the run', t.errors, []);
  }

  {
    // Nothing to do is worth saying rather than silently doing nothing.
    const t = await boot({ result: null });
    await openSettings(t.doc);
    // Fill the one incomplete game first, so nothing is left.
    t.doc.getElementById('fetch-all-metadata').click();
    await sleep(500);
    t.doc.getElementById('fetch-all-metadata').click();
    await sleep(200);
    check('a complete library says so',
          t.doc.getElementById('metadata-status').textContent,
          'Every game already has a description and a cover.');
  }

  {
    // A bad key fails every remaining game, so the run stops.
    const t = await boot({ fails: true });
    await openSettings(t.doc);
    t.doc.getElementById('fetch-all-metadata').click();
    await sleep(400);
    const status = t.doc.getElementById('metadata-status').textContent;
    check('a run that stopped says why', status.includes('Stopped:'), true);
    check('...naming the reason', status.includes('rejected the API key'), true);
    check('...and the button is usable again',
          t.doc.getElementById('fetch-all-metadata').disabled, false);
    check('no uncaught errors when a run stops', t.errors, []);
  }

  finish();
})();
