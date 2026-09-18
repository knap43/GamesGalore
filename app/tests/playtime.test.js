// Playtime, which the UI has offered to sort by since the first
// version and which did nothing at all for a real library: the sorts,
// what the cards and the detail header show, and the live update when
// a session ends.
const { boot: bootApp, check, sleep, finish, createRuntime, openGame } = require('./harness');

const DAY = 86400;
const now = () => Math.floor(Date.now() / 1000);

const game = (id, title, platform) => ({
  id, title, platform, release_year: 2020, description: 'A game.',
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: 10 }],
  screenshots: [], cover: null, trailer: null,
});

const LIBRARY = [
  game('PC/Hollow Meridian', 'Hollow Meridian', 'PC'),
  game('PC/Tidebreaker', 'Tidebreaker', 'PC'),
  game('Switch/Bramblewood', 'Bramblewood', 'Switch'),
];

async function boot(playtime = {}) {
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: {}, launch_overrides: {}, prefix_root: '',
    save_sync: { enabled: false, device_name: 'test', switch_data_dir: '', title_ids: {}, token: '' },
  };
  const rt = createRuntime();
  const invoke = async (cmd, args) => {
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'get_cached_library': return [];
      case 'fetch_library': return LIBRARY.map(g => ({ ...g }));
      case 'get_install_states': return {
        'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' },
        'PC/Tidebreaker': { status: 'installed', local_dir: '/games/PC/Tidebreaker' },
        'Switch/Bramblewood': { status: 'installed', local_dir: '/games/Switch/Bramblewood' },
      };
      case 'get_playtime': return playtime;
      case 'list_launch_candidates': return [];
      default: return null;
    }
  };
  const t = await bootApp({ invoke, listen: rt.listen });
  return { ...t, emit: rt.emit, settings };
}

const titlesInOrder = doc =>
  [...doc.querySelectorAll('.card .title')].map(e => e.textContent.trim());
const sub = (doc, title) => {
  const card = [...doc.querySelectorAll('.card')].find(c => c.textContent.includes(title));
  return card ? card.querySelector('.sub').textContent : null;
};
const setSort = async (doc, win, value) => {
  doc.querySelector(`.sort-pill[data-sort="${value}"]`).click();
  await sleep(120);
};

(async () => {
  const played = {
    'PC/Hollow Meridian': { seconds: 76 * 3600, last_played: now() - 20 * DAY, sessions: 40 },
    'PC/Tidebreaker': { seconds: 2 * 3600, last_played: now() - 1 * DAY, sessions: 3 },
    // Never played: no entry at all, which is what the backend returns.
  };
  const { doc, win, errors, emit } = await boot(played);

  check('a played game shows its hours on the card',
        sub(doc, 'Hollow Meridian').includes('76h played'), true);
  check('an unplayed one shows no hours at all',
        sub(doc, 'Bramblewood').includes('played'), false);

  await setSort(doc, win, 'playtime');
  check('sorting by playtime puts the most played first',
        titlesInOrder(doc)[0], 'Hollow Meridian');
  check('...and an unplayed game last',
        titlesInOrder(doc)[2], 'Bramblewood');

  await setSort(doc, win, 'recent');
  check('sorting by recent puts the most recently played first',
        titlesInOrder(doc)[0], 'Tidebreaker');
  check('...and a game nobody has played last, not first',
        titlesInOrder(doc)[2], 'Bramblewood');

  // The detail header shows both numbers.
  await openGame(doc, 'Hollow Meridian');
  const meta = doc.getElementById('detail-meta').textContent;
  check('the detail view shows the hours', meta.includes('76h played'), true);
  check('...and when it was last played', meta.includes('Last played'), true);

  // A finished session arrives as an event, so quitting a game updates
  // the sort without a restart.
  emit('playtime:changed', ['PC/Hollow Meridian',
                            { seconds: 80 * 3600, last_played: now(), sessions: 41 }]);
  await sleep(150);
  check('a finished session updates the open detail view',
        doc.getElementById('detail-meta').textContent.includes('80h played'), true);

  doc.getElementById('detail-back').click();
  await sleep(150);
  check('...and the card behind it',
        sub(doc, 'Hollow Meridian').includes('80h played'), true);
  await setSort(doc, win, 'recent');
  check('...and where it sorts', titlesInOrder(doc)[0], 'Hollow Meridian');

  check('no uncaught errors', errors, []);

  // A short session is not playtime, and the backend says so by
  // sending an entry whose seconds never moved.
  {
    const t = await boot({ 'PC/Tidebreaker': { seconds: 0, last_played: now(), sessions: 1 } });
    check('a game opened and closed immediately shows no hours',
          sub(t.doc, 'Tidebreaker').includes('played'), false);
    await setSort(t.doc, t.win, 'recent');
    check('...but is still the most recently played',
          titlesInOrder(t.doc)[0], 'Tidebreaker');
    check('no uncaught errors on the short-session path', t.errors, []);
  }

  // === the sort control itself ========================================
  // It was removed when the only options that worked were alphabetical
  // and platform; a native <select> is also unstyleable in WebKitGTK,
  // which is why this is pills rather than a dropdown.
  {
    const t = await boot(played);
    const pills = [...t.doc.querySelectorAll('.sort-pill')].map(p => p.dataset.sort);
    check('every order is offered', pills, ['recent', 'playtime', 'alpha', 'platform']);
    check('the current one is marked',
          t.doc.querySelector('.sort-pill.active').dataset.sort, 'alpha');

    t.doc.querySelector('.sort-pill[data-sort="playtime"]').click();
    await sleep(150);
    check('picking one moves the highlight',
          t.doc.querySelector('.sort-pill.active').dataset.sort, 'playtime');
    check('...keeps focus on the pill rather than dropping it',
          t.doc.activeElement.dataset.sort, 'playtime');
    check('...and is remembered for next launch', t.settings.sort, 'playtime');

    // The chain: pills sit above the grid and below the search box.
    const press = key => t.doc.dispatchEvent(
      new t.win.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
    press('ArrowDown');
    check('arrowing down from a pill reaches the grid',
          t.doc.activeElement.classList.contains('card'), true);
    press('ArrowUp');
    check('...and back up to the pills',
          t.doc.activeElement.classList.contains('sort-pill'), true);
    check('no uncaught errors around the sort control', t.errors, []);
  }

  finish();
})();
