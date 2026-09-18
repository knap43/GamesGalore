// The cloud-save flow: syncing before launch, uploading after the
// session ends, the conflict prompt in between, and how a Title ID is
// resolved without the user ever having to know there is one.
const { boot: bootApp, check, sleep, finish, createRuntime, openGame } = require('./harness');

async function boot(opts = {}) {
  const calls = [];
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: { PS1: {command:'a',args_prefix:[],version_flag:'-v'},
                 PS2: {command:'b',args_prefix:[],version_flag:'-v'},
                 PC:  {command:'wine',args_prefix:[],version_flag:'-v'},
                 Switch:{command:'eden',args_prefix:[],version_flag:'-v'} },
    launch_overrides: {}, prefix_root: '',
    ...(opts.defaultSettings ? {} : { save_sync: {
      enabled: opts.syncEnabled !== false,
      device_name: 'laptop',
      switch_data_dir: opts.dataDir === undefined ? '/home/me/.local/share/eden' : opts.dataDir,
      title_ids: opts.titleIds || { 'Switch/198X': '0100AAA000BBB000' },
    } }),
  };
  // An older settings.json has no save_sync key at all; the frontend
  // must fall back to its own default rather than to undefined.
  if (opts.defaultSettings) delete settings.save_sync;

  const rt = createRuntime();

  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'get_cached_library': return [];
      case 'fetch_library': return [
        { id:'Switch/198X', title:'198X', platform:'Switch', description:'d',
          files:[{filename:'base.nsp', size_bytes:1000}], screenshots:[], cover:null, trailer:null },
        { id:'PS1/Static Choir', title:'Static Choir', platform:'PS1', description:'d',
          files:[{filename:'sc.cue', size_bytes:300}], screenshots:[], cover:null, trailer:null },
        { id:'PC/ULTRAKILL', title:'ULTRAKILL', platform:'PC', description:'d',
          files:[{filename:'game.exe', size_bytes:900}], screenshots:[], cover:null, trailer:null },
      ];
      case 'get_install_states': return opts.nothingInstalled ? {} : {
        'Switch/198X': { status:'installed', local_dir:'/games/Switch/198X' },
        'PS1/Static Choir': { status:'installed', local_dir:'/games/PS1/Static Choir' },
        'PC/ULTRAKILL': { status:'installed', local_dir:'/games/PC/ULTRAKILL' },
      };
      case 'list_launch_candidates': return [];
      case 'list_switch_title_ids':
        // Two calls per session: before launch, then after exit.
        if (opts.idsAfterSession && calls.filter(c => c[0] === 'list_switch_title_ids').length > 1) {
          return opts.idsAfterSession;
        }
        return opts.availableIds || ['0100AAA000BBB000', '0100CCC000DDD000'];
      case 'detect_switch_data_dir': return opts.detectedDir ?? null;
      case 'set_switch_title_id': return null;
      case 'save_status': return opts.status || { state:'in_sync', local_modified:100,
                                                  local_bytes:10, latest:null, unavailable:null,
                                                  title_id: opts.resolvedId ?? null,
                                                  unsynced: opts.unsynced || [] };
      case 'download_save':
        if (opts.downloadFails) throw new Error('server unreachable');
        return null;
      case 'upload_save':
        if (opts.uploadFails) throw new Error('disk full');
        return { version:'v1', size_bytes:10, unchanged: false };
      case 'launch_game': return null;
      default: return null;
    }
  };

  const { win, doc, errors } = await bootApp({
    invoke, listen: rt.listen, confirm: opts.confirmAnswer !== false, settle: 450,
  });
  return { win, doc, calls, settings, errors, emit: rt.emit };
}

(async () => {
  // === in sync: launch is not delayed or altered ======================
  let t = await boot();
  check('no uncaught errors on boot', t.errors, []);
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('an in-sync game checks status before launching',
        t.calls.some(c => c[0] === 'save_status'), true);
  check('...restores nothing', t.calls.some(c => c[0] === 'download_save'), false);
  check('...and still launches', t.calls.some(c => c[0] === 'launch_game'), true);
  check('launch now passes the game id for its prefix and exit event',
        t.calls.filter(c => c[0] === 'launch_game').pop()[1].gameId, 'Switch/198X');

  // === remote only: restored without asking ===========================
  t = await boot({ status: { state:'remote_only', local_modified:0, local_bytes:0,
                             latest:{version:'v9',saved_at:'500',device:'desktop'}, unavailable:null } });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('a save only on the server is restored without a prompt',
        t.calls.some(c => c[0] === 'download_save'), true);
  check('...and the game launches after it', t.calls.some(c => c[0] === 'launch_game'), true);

  // === remote newer, user accepts =====================================
  const conflict = { state:'remote_newer', local_modified:100, local_bytes:10,
                     latest:{version:'v9',saved_at:'500',device:'desktop'}, unavailable:null };
  t = await boot({ status: conflict, confirmAnswer: true });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('a newer remote save prompts, and accepting restores it',
        t.calls.some(c => c[0] === 'download_save'), true);
  check('...then launches', t.calls.some(c => c[0] === 'launch_game'), true);

  // === remote newer, user declines ====================================
  t = await boot({ status: conflict, confirmAnswer: false });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('declining the prompt restores nothing',
        t.calls.some(c => c[0] === 'download_save'), false);
  check('...but still launches with the local save',
        t.calls.some(c => c[0] === 'launch_game'), true);

  // === a failed restore must NOT launch ===============================
  t = await boot({ status: conflict, confirmAnswer: true, downloadFails: true });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  check('a failed restore blocks the launch rather than overwriting the newer save',
        t.calls.some(c => c[0] === 'launch_game'), false);
  check('...and says why',
        t.doc.getElementById('install-error').textContent.includes('Could not restore'), true);

  // === upload on exit =================================================
  t = await boot();
  await openGame(t.doc, '198X');
  t.emit('game:exited', 'Switch/198X');
  await sleep(120);
  check('quitting a game uploads its save', t.calls.some(c => c[0] === 'upload_save'), true);
  check('...with the right game and platform',
        [t.calls.filter(c=>c[0]==='upload_save').pop()[1].gameId,
         t.calls.filter(c=>c[0]==='upload_save').pop()[1].platform], ['Switch/198X','Switch']);

  // === an unsupported platform is left alone ==========================
  t = await boot();
  await openGame(t.doc, 'Static Choir');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('a PS1 game is not save-synced at all',
        t.calls.some(c => c[0] === 'save_status'), false);
  check('...and launches normally', t.calls.some(c => c[0] === 'launch_game'), true);
  t.emit('game:exited', 'PS1/Static Choir');
  await sleep(80);
  check('...and uploads nothing on exit', t.calls.some(c => c[0] === 'upload_save'), false);

  // === sync switched off ==============================================
  t = await boot({ syncEnabled: false });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('with sync off nothing is checked', t.calls.some(c => c[0] === 'save_status'), false);
  t.emit('game:exited', 'Switch/198X');
  await sleep(80);
  check('...and nothing is uploaded on exit', t.calls.some(c => c[0] === 'upload_save'), false);

  // === unconfigured game degrades quietly =============================
  t = await boot({ status: { state:'none', local_modified:0, local_bytes:0, latest:null,
                             unavailable:'this game has no Title ID mapped in Settings' } });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('an unmapped game still launches', t.calls.some(c => c[0] === 'launch_game'), true);
  check('...and is not restored over', t.calls.some(c => c[0] === 'download_save'), false);

  // === settings: no per-game rows any more ============================
  t = await boot({ titleIds: {} });
  t.doc.querySelector('#settings-nav .nav-item').click();
  await sleep(140);
  check('the per-game Title ID rows are gone entirely',
        t.doc.querySelectorAll('.title-id-select').length, 0);
  check('no native select survives anywhere in settings',
        t.doc.querySelectorAll('#settings-backdrop select').length, 0);
  check('one status line reports identification instead',
        t.doc.getElementById('switch-saves-status').textContent,
        '0 of 1 installed Switch games identified. '
        + 'The rest are worked out when you next play or sync them.');

  t = await boot();
  t.doc.querySelector('#settings-nav .nav-item').click();
  await sleep(140);
  check('...and says so plainly once everything is identified',
        t.doc.getElementById('switch-saves-status').textContent,
        'All 1 installed Switch games identified.');

  t = await boot({ dataDir: '' });
  t.doc.querySelector('#settings-nav .nav-item').click();
  await sleep(140);
  check('with no emulator directory it asks for one rather than listing games',
        t.doc.getElementById('switch-saves-status').textContent,
        'No emulator data directory found yet — choose one above.');

  const toggle = t.doc.getElementById('save-sync-toggle');
  check('the toggle is the app\'s own switch component, not a button label',
        [toggle.className, toggle.getAttribute('role')], ['toggle-switch on', 'switch']);
  check('the switch reads on for the loaded state', toggle.getAttribute('aria-checked'), 'true');
  check('the state line says what on actually means',
        t.doc.getElementById('save-sync-state').textContent.includes('mirrored to the server'), true);
  toggle.click();
  await sleep(100);
  check('toggling off persists', t.settings.save_sync.enabled, false);
  check('...and the switch follows', toggle.getAttribute('aria-checked'), 'false');
  check('...and the state line makes clear saving still happens locally',
        t.doc.getElementById('save-sync-state').textContent,
        'Saves are kept on this machine only. Nothing is uploaded or restored.');
  check('no uncaught errors across the settings flow', t.errors, []);

  // === the data directory fills itself in =============================
  t = await boot({ dataDir: '', detectedDir: '/home/me/.local/share/eden' });
  await sleep(120);
  check('an empty data directory is found automatically',
        t.doc.getElementById('switch-data-dir-input').value, '/home/me/.local/share/eden');
  check('...and persisted', t.settings.save_sync.switch_data_dir, '/home/me/.local/share/eden');

  t = await boot({ detectedDir: '/somewhere/else' });
  await sleep(120);
  check('a directory already chosen is never second-guessed',
        t.settings.save_sync.switch_data_dir, '/home/me/.local/share/eden');

  // === identifying a game by watching a session =======================
  t = await boot({ titleIds: {},
                   availableIds: ['0100AAA000BBB000'],
                   idsAfterSession: ['0100AAA000BBB000', '0100DDD000DDD000'] });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  check('an unidentified game is snapshotted before it runs',
        t.calls.filter(c => c[0] === 'list_switch_title_ids').length, 1);
  t.emit('game:exited', 'Switch/198X');
  await sleep(160);
  check('the directory that appeared is recorded as the game\'s',
        t.calls.filter(c => c[0] === 'set_switch_title_id').pop()[1],
        { gameId: 'Switch/198X', titleId: '0100DDD000DDD000' });

  // === ambiguity is left alone ========================================
  t = await boot({ titleIds: {},
                   availableIds: ['0100AAA000BBB000'],
                   idsAfterSession: ['0100AAA000BBB000', '0100BBB000BBB000', '0100CCC000CCC000'] });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  t.emit('game:exited', 'Switch/198X');
  await sleep(160);
  check('two new directories at once identify nothing, rather than guessing',
        t.calls.some(c => c[0] === 'set_switch_title_id'), false);

  // === an already-identified game is not re-snapshotted ===============
  t = await boot();
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  check('a game that already has an id does no extra work',
        t.calls.filter(c => c[0] === 'list_switch_title_ids').length, 0);

  // === the new default ================================================
  t = await boot({ defaultSettings: true });
  check('a settings file with no save_sync block defaults to on',
        t.doc.getElementById('save-sync-toggle').getAttribute('aria-checked'), 'true');
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(120);
  check('...so a fresh install syncs without anyone finding a setting',
        t.calls.some(c => c[0] === 'save_status'), true);

  // === a malformed status must not strand the launch ==================
  t = await boot({ status: null });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  check('a null save status still lets the game launch',
        t.calls.some(c => c[0] === 'launch_game'), true);
  check('...without an uncaught error', t.errors, []);

  // === the save notice is good news, and looks like it ================
  t = await boot();
  await openGame(t.doc, '198X');
  t.emit('game:exited', 'Switch/198X');
  await sleep(140);
  const note = t.doc.getElementById('install-error');
  check('an uploaded save says so', note.textContent, 'Save uploaded.');
  check('...in green, not as an error', note.classList.contains('is-good'), true);

  t = await boot({ status: { state:'remote_only', local_modified:0, local_bytes:0,
                             latest:{version:'v9',saved_at:'500',device:'desktop'}, unavailable:null } });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(140);
  check('a restore is also good news',
        t.doc.getElementById('install-error').classList.contains('is-good'), true);

  t = await boot({ status: { state:'remote_newer', local_modified:100, local_bytes:10,
                             latest:{version:'v9',saved_at:'500',device:'desktop'}, unavailable:null },
                   confirmAnswer: true, downloadFails: true });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(160);
  check('a failed restore stays red',
        t.doc.getElementById('install-error').classList.contains('is-good'), false);

  // === the installed filter is on at launch ===========================
  t = await boot();
  const statusItem = () => t.doc.querySelector('#status-nav .nav-item');
  check('the installed filter starts on', statusItem().classList.contains('active'), true);
  check('...and the grid is filtered to installed games',
        t.doc.querySelectorAll('.card').length, 3);
  // It must survive the re-render that every install-status change causes.
  t.emit('game:exited', 'Switch/198X');
  await sleep(140);
  check('...and survives a status re-render',
        statusItem().classList.contains('active'), true);
  statusItem().click();
  await sleep(100);
  check('turning it off still works', statusItem().classList.contains('active'), false);

  t = await boot({ nothingInstalled: true });
  check('with nothing installed the filter stays off',
        t.doc.querySelector('#status-nav .nav-item').classList.contains('active'), false);
  check('...so the whole library is still visible',
        t.doc.querySelectorAll('.card').length, 3);

  // === an id the backend resolved is not re-derived every launch =====
  t = await boot({ titleIds: {}, resolvedId: '0100AAA000BBB000' });
  await openGame(t.doc, '198X');
  t.doc.getElementById('cta-button').click();
  await sleep(160);
  check('an id resolved from the game\'s files is recorded here too',
        t.settings.save_sync.title_ids['Switch/198X'] ?? 'not recorded', '0100AAA000BBB000');
  check('...so the launch does no session-watching work at all',
        t.calls.filter(c => c[0] === 'list_switch_title_ids').length, 0);

  // === the save token =================================================
  t = await boot();
  t.doc.querySelector('.nav-item[data-type="settings"]').click();
  await sleep(150);
  const tokenInput = t.doc.getElementById('save-token-input');
  check('the token field is on the settings screen', !!tokenInput, true);
  check('...and is not shown in the clear', tokenInput.type, 'password');

  tokenInput.value = 's3cret-token';
  tokenInput.dispatchEvent(new t.win.Event('change'));
  await sleep(150);
  check('...and is persisted with the rest of the save settings',
        t.settings.save_sync.token, 's3cret-token');

  // === saves written outside the prefix ===============================
  // Wine links a prefix's Documents out to the real home directory and
  // the archive refuses to follow it, so a game saving there is left
  // behind with nothing about the sync looking wrong.
  t = await boot({ unsynced: ['Documents', 'Saved Games'] });
  await openGame(t.doc, 'ULTRAKILL');
  await sleep(200);
  const prefixNote = t.doc.getElementById('install-error');
  check('a prefix that links its save folders out says so', prefixNote.style.display, 'block');
  check('...naming the folders', prefixNote.textContent.includes('Documents and Saved Games'), true);
  check('...and the remedy', prefixNote.textContent.includes('winecfg'), true);
  check('...in the neutral tone, not as a failure',
        [prefixNote.classList.contains('is-info'), prefixNote.classList.contains('is-good')], [true, false]);

  t = await boot();
  await openGame(t.doc, 'ULTRAKILL');
  await sleep(200);
  check('a prefix that keeps its own folders says nothing',
        t.doc.getElementById('install-error').style.display, 'none');

  t = await boot({ unsynced: ['Documents'] });
  await openGame(t.doc, '198X');
  await sleep(200);
  check('and a Switch game is never asked about prefixes at all',
        t.doc.getElementById('install-error').style.display, 'none');

  finish();
})();
