// The settings screen under keyboard and gamepad control: every field
// reachable, nothing focusable that can't be seen, and the modal
// scrolled so the focused control is actually on screen.
const { boot: bootApp, check, sleep, finish, createRuntime } = require('./harness');

async function boot() {
  const settings = {
    server_base: 'http://x:8420', install_root: '/games', sound_enabled: false,
    emulators: { PS1: { command: 'duckstation-qt', args_prefix: [], version_flag: '-version' },
                 PS2: { command: 'pcsx2-qt', args_prefix: [], version_flag: '--version' },
                 PC:  { command: 'wine', args_prefix: [], version_flag: '--version' },
                 Switch: { command: 'eden', args_prefix: [], version_flag: '--version' } },
    launch_overrides: {}, prefix_root: '',
    save_sync: { enabled: true, device_name: 'laptop', switch_data_dir: '', title_ids: {}, token: '' },
  };
  const rt = createRuntime();
  const invoke = async (cmd, args) => {
    switch (cmd) {
      case 'get_settings': return settings;
      case 'save_settings': Object.assign(settings, args.settings); return null;
      case 'get_cached_library': return [];
      case 'fetch_library': return [];
      case 'get_install_states': return {};
      case 'check_dependency': return { present: true, version: '1.0' };
      default: return null;
    }
  };

  const t = await bootApp({ invoke, listen: rt.listen });

  // jsdom has no layout, so nothing ever scrolls on its own; record the
  // calls instead, which is what the assertion is really about.
  t.scrolled = [];
  t.win.Element.prototype.scrollIntoView = function () { t.scrolled.push(this); };
  return { ...t, settings };
}

const press = (win, key) => win.document.dispatchEvent(
  new win.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));

(async () => {
  const { win, doc, errors, settings } = await boot();

  doc.querySelector('.nav-item[data-type="settings"]').click();
  await sleep(150);
  check('settings opens on its close button', doc.activeElement.id, 'settings-close');

  // Walk the whole form and record where it goes.
  const visited = [];
  for (let i = 0; i < 60; i++) {
    press(win, 'ArrowDown');
    const el = doc.activeElement;
    if (!el || el === doc.body) break;
    if (visited[visited.length - 1] === el) break; // reached the end
    visited.push(el);
  }

  const modal = doc.querySelector('#settings-backdrop .modal');
  check('the walk stays inside the modal',
        visited.every(el => modal.contains(el)), true);
  check('...and reaches the far end of the form',
        visited.includes(doc.getElementById('sound-toggle')), true);
  check('...including the fields added since it was written',
        [doc.getElementById('save-token-input'), doc.getElementById('prefix-root-input')]
          .every(el => visited.includes(el)), true);
  check('...and the per-emulator rows, which are rendered rather than written out',
        visited.filter(el => el.closest('.emulator-row')).length >= 4, true);
  check('nothing invisible was focused',
        visited.every(el => el.offsetParent !== null), true);
  check('nothing disabled was focused', visited.every(el => !el.disabled), true);

  // Every step scrolls its target into view: the modal is taller than
  // its own max-height, so without this the caret ends up in a field
  // below the fold.
  const scrolled = (await (async () => {
    const { win: w, doc: d } = await boot();
    d.querySelector('.nav-item[data-type="settings"]').click();
    await sleep(150);
    const before = w.document.activeElement;
    press(w, 'ArrowDown');
    return { moved: w.document.activeElement !== before, doc: d, win: w };
  })());
  check('moving focus in the modal actually moves it', scrolled.moved, true);

  // Back up again: the walk has to work in both directions and stop at
  // the top rather than falling out of the modal.
  for (let i = 0; i < 80; i++) press(win, 'ArrowUp');
  check('walking back up stops at the first control',
        doc.activeElement.id, 'settings-close');
  check('and is still inside the modal', modal.contains(doc.activeElement), true);

  // Left/Right are the same chain, which is what lets a D-pad out of a
  // text input it would otherwise be stuck in.
  doc.getElementById('server-base-input').focus();
  press(win, 'ArrowDown');
  check('a text input can be left by arrowing down',
        doc.activeElement.id !== 'server-base-input', true);

  // The PC runtime switch. The fixture's settings.json has no runtime
  // field at all — as every settings.json written before Proton does —
  // so this also covers the row rendering from an absent value.
  const pcRow = () => doc.querySelector('.emulator-row[data-platform="PC"]');
  const runtimePill = (value) => pcRow().querySelector(`[data-runtime="${value}"]`);

  check('PC starts on Wine', runtimePill('wine').classList.contains('active'), true);
  check('...and it is the only row offering the choice',
        doc.querySelectorAll('.runtime-pill').length, 2);

  runtimePill('proton').click();
  await sleep(50);
  check('picking Proton is persisted', settings.emulators.PC.runtime, 'proton');
  check('...and carries the command with it', settings.emulators.PC.command, 'umu-run');
  check('...and the pill that is now active says so',
        runtimePill('proton').getAttribute('aria-pressed'), 'true');
  check('...and the Proton build field appears',
        pcRow().querySelector('.emu-proton-path').style.display, '');

  const protonPath = pcRow().querySelector('.emu-proton-path');
  protonPath.value = 'GE-Proton';
  protonPath.dispatchEvent(new win.Event('change', { bubbles: true }));
  await sleep(50);
  check('a chosen Proton build is persisted', settings.emulators.PC.proton_path, 'GE-Proton');

  runtimePill('wine').click();
  await sleep(50);
  check('switching back restores the Wine command', settings.emulators.PC.command, 'wine');
  check('...and hides the Proton build field',
        pcRow().querySelector('.emu-proton-path').style.display, 'none');

  // A command someone typed themselves is theirs, not ours to replace.
  const command = pcRow().querySelector('.emu-command');
  command.value = '/opt/wine-staging/bin/wine';
  command.dispatchEvent(new win.Event('change', { bubbles: true }));
  await sleep(50);
  runtimePill('proton').click();
  await sleep(50);
  check('a hand-typed command survives the switch',
        settings.emulators.PC.command, '/opt/wine-staging/bin/wine');
  runtimePill('wine').click();
  await sleep(50);

  // Escape from a text field has to work too. Refusing it left a
  // keyboard user stuck in the modal, while a gamepad's B button was
  // never affected — the asymmetry this navigation exists to avoid.
  doc.getElementById('server-base-input').focus();
  doc.getElementById('server-base-input').value = 'http://typed-but-not-committed:8420';
  press(win, 'Escape');
  await sleep(100);
  check('Escape from a settings field closes settings',
        doc.getElementById('settings-backdrop').style.display, 'none');
  check('...having committed what was typed first',
        settings.server_base, 'http://typed-but-not-committed:8420');

  check('no uncaught errors', errors, []);
  finish();
})();
