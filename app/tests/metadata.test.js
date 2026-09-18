// The optional game.json a library can carry: genre on the cards and
// in the detail header, and the genre filter that appears only when
// there is something to filter by.
const { boot: bootApp, check, sleep, finish, createRuntime, openGame } = require('./harness');

const game = (id, title, platform, extra = {}) => ({
  id, title, platform, release_year: 2020, description: 'A game.',
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: 10 }],
  screenshots: [], cover: null, trailer: null,
  genre: null, tags: [], players: null,
  ...extra,
});

const WITH_GENRES = [
  game('PC/Hollow Meridian', 'Hollow Meridian', 'PC', { genre: 'RPG', tags: ['moody'], players: 1 }),
  game('PC/Voltgrid Arena', 'Voltgrid Arena', 'PC', { genre: 'Sports', players: 4 }),
  game('Switch/Bramblewood', 'Bramblewood', 'Switch', { genre: 'RPG' }),
  game('PS1/Nebula Drift', 'Nebula Drift', 'PS1'), // no sidecar at all
];

async function boot(library) {
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: {}, launch_overrides: {}, prefix_root: '', sort: 'alpha',
    save_sync: { enabled: false, device_name: 'test', switch_data_dir: '', title_ids: {}, token: '' },
  };
  const rt = createRuntime();
  const invoke = async (cmd, args) => {
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'get_cached_library': return [];
      case 'fetch_library': return library.map(g => ({ ...g }));
      case 'get_install_states': return {};
      case 'get_playtime': return {};
      case 'list_launch_candidates': return [];
      default: return null;
    }
  };
  return bootApp({ invoke, listen: rt.listen });
}

const genreItems = doc =>
  [...doc.querySelectorAll('#genre-nav .nav-item')].map(el => el.dataset.value);
const titles = doc =>
  [...doc.querySelectorAll('.card .title')].map(e => e.textContent.trim()).sort();

(async () => {
  const { doc, errors } = await boot(WITH_GENRES);

  check('every genre in the library is offered', genreItems(doc), ['RPG', 'Sports']);
  check('...counted', doc.querySelector('#genre-nav .nav-item .count').textContent, '2');

  const card = [...doc.querySelectorAll('.card')].find(c => c.textContent.includes('Hollow Meridian'));
  check('a genre shows on the card', card.querySelector('.sub').textContent.includes('RPG'), true);

  doc.querySelector('#genre-nav .nav-item[data-value="RPG"]').click();
  await sleep(150);
  check('filtering by genre narrows the grid',
        titles(doc), ['Bramblewood', 'Hollow Meridian']);
  check('...and the filter shows as active',
        doc.querySelector('#genre-nav .nav-item[data-value="RPG"]').classList.contains('active'), true);

  doc.querySelector('#genre-nav .nav-item[data-value="RPG"]').click();
  await sleep(150);
  check('turning it off restores the rest', titles(doc).length, 4);

  await openGame(doc, 'Hollow Meridian');
  check('the detail header shows the genre',
        doc.getElementById('detail-meta').textContent.includes('RPG'), true);
  check('no uncaught errors', errors, []);

  // The common case: a library nobody has annotated.
  {
    const plain = WITH_GENRES.map(g => ({ ...g, genre: null }));
    const t = await boot(plain);
    check('with no genres anywhere the section is not rendered at all',
          t.doc.getElementById('genre-nav').innerHTML, '');
    check('...and every game is still shown', titles(t.doc).length, 4);
    check('no uncaught errors without metadata', t.errors, []);
  }

  finish();
})();
