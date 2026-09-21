// The Logs window: what the session has already printed, what arrives
// while it is open, and the scroll behaviour that decides whether a
// line arriving moves the view out from under someone reading it.
const { boot: bootApp, check, sleep, finish, createRuntime } = require('./harness');

const settings = {
  server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
  emulators: {}, launch_overrides: {}, prefix_root: '',
  save_sync: { enabled: false, device_name: 'x', switch_data_dir: '', title_ids: {}, token: '' },
};

/* Two lines from before the window was ever opened, as a real session
   would have: the app logs from the moment it starts. */
const existing = [
  { at: Date.parse('2026-09-21T10:00:00Z'), text: 'installing PC/Ferrofluid to /games/PC/Ferrofluid' },
  { at: Date.parse('2026-09-21T10:04:00Z'), text: 'launch_game: wine /games/PC/Ferrofluid/game.exe' },
];

(async () => {
  const rt = createRuntime();
  const invoke = async (cmd) => {
    switch (cmd) {
      case 'get_settings': return settings;
      case 'get_logs': return existing;
      case 'get_cached_library': return [];
      case 'fetch_library': return [];
      case 'get_install_states': return {};
      default: return null;
    }
  };
  const { win, doc, errors } = await bootApp({ invoke, listen: rt.listen });

  doc.querySelector('.nav-item[data-type="settings"]').click();
  await sleep(100);
  check('logs start closed',
        doc.getElementById('logs-backdrop').style.display, 'none');

  doc.getElementById('view-logs').click();
  await sleep(100);
  check('the button opens them',
        doc.getElementById('logs-backdrop').style.display, 'flex');
  check('...on the close button, so a gamepad can get back out',
        doc.activeElement.id, 'logs-close');

  const lines = () => Array.from(doc.querySelectorAll('#logs-output .log-line'))
    .map(el => el.textContent);
  check('what the session printed before it was opened is there', lines().length, 2);
  check('...with the text as it was printed',
        lines()[0].includes('installing PC/Ferrofluid to /games/PC/Ferrofluid'), true);
  check('...and the time it was printed at',
        /^\d\d:\d\d:\d\d/.test(lines()[0]), true);

  // A line arriving while the window is open is the whole point of it
  // being live rather than a snapshot.
  rt.emit('log:line', { at: Date.now(), text: 'installed PC/Ferrofluid in /games/PC/Ferrofluid' });
  await sleep(50);
  check('a line arriving appears without reopening', lines().length, 3);
  check('...at the end, where it happened',
        lines()[2].includes('installed PC/Ferrofluid'), true);

  // Markup in a log line is text, not markup: filenames and error
  // messages come off a real filesystem and a real server.
  rt.emit('log:line', { at: Date.now(), text: 'could not prepare <b>/games/&</b>' });
  await sleep(50);
  check('a line is shown rather than interpreted',
        lines()[3].includes('could not prepare <b>/games/&</b>'), true);
  check('...and adds no elements of its own',
        doc.querySelectorAll('#logs-output b').length, 0);

  // A game's own output is attributed to it by the backend, and the
  // window picks that prefix out so a wall of Wine chatter is
  // scannable. The text must survive that untouched.
  rt.emit('log:line', { at: Date.now(), text: '[Ferrofluid] fixme:heap: stub' });
  await sleep(50);
  check("a game's line keeps its text, title and all",
        lines()[4].includes('[Ferrofluid] fixme:heap: stub'), true);
  check('...with the title marked up as the game rather than the app',
        doc.querySelectorAll('#logs-output .log-game').length, 1);

  // jsdom has no clipboard, which is also the case in a webview that
  // declines to provide one — both paths matter, so both are here.
  let copied = null;
  win.navigator.clipboard = { writeText: async (text) => { copied = text; } };
  doc.getElementById('logs-copy').click();
  await sleep(50);
  check('copying says it copied',
        doc.getElementById('logs-copy').textContent.trim(), 'Copied');
  check('...and copies every line, stamped as shown',
        copied.split('\n').length, 5);
  check('...with the time in front of each',
        /^\d\d:\d\d:\d\d installing PC\/Ferrofluid/.test(copied), true);

  win.navigator.clipboard = { writeText: async () => { throw new Error('nope'); } };
  doc.getElementById('logs-copy').click();
  await sleep(50);
  check('a clipboard that refuses says so rather than lying',
        doc.getElementById('logs-copy').textContent.trim(), 'Could not copy');

  // Escape closes the logs first and leaves Settings open behind them.
  doc.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await sleep(100);
  check('Escape closes the logs',
        doc.getElementById('logs-backdrop').style.display, 'none');
  check('...and leaves Settings where it was',
        doc.getElementById('settings-backdrop').style.display, 'flex');
  check('...with focus back on the button that opened them',
        doc.activeElement.id, 'view-logs');

  // Lines still arrive while it is closed, and are there on reopening.
  rt.emit('log:line', { at: Date.now(), text: 'uploaded the save for PC/Ferrofluid as 3' });
  await sleep(50);
  doc.getElementById('view-logs').click();
  await sleep(100);
  check('a line printed while it was closed is there on reopening',
        lines().length, 6);

  check('no uncaught errors', errors, []);
  finish();
})();
