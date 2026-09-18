/*
 * Shared scaffolding for the frontend suites.
 *
 * The app is a single HTML file with no build step, which means the
 * honest way to test it is to load that exact file into a real DOM and
 * drive it through the same events a person would generate. jsdom
 * supplies the DOM; this module supplies everything jsdom itself
 * doesn't, plus a stand-in for the Tauri runtime the app expects to
 * find on `window`.
 *
 * Every suite boots through here so that a gap in the environment is
 * fixed once rather than in four places — and so that a suite's own
 * code is only the mock backend and the assertions, which is the part
 * worth reading.
 */

const fs = require('fs');
const path = require('path');
const { JSDOM } = require('jsdom');

const APP_HTML = path.join(__dirname, '..', 'frontend', 'index.html');
const DEMO_HTML = path.join(__dirname, '..', '..', '..', '..', 'docs', 'index.html');

let fails = 0;
let total = 0;

/* Deep-compares through JSON so arrays and objects can be asserted
   directly, and prints both sides on a failure — a bare "expected
   true" tells you nothing at three in the morning. */
function check(label, got, want) {
  total++;
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) fails++;
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) console.log(`        got=${JSON.stringify(got)}\n       want=${JSON.stringify(want)}`);
  return ok;
}

/* The app is asynchronous throughout — settings, catalog, install
   events, save sync — and none of it is exposed as a promise a test
   could await. Waiting a beat after each interaction is the honest
   way to test it; the alternative is exporting internals purely for
   the tests, which would be testing a different program. */
const sleep = ms => new Promise(r => setTimeout(r, ms));

function finish() {
  console.log(fails ? `\n${fails} of ${total} FAILED` : `\nALL PASS (${total} checks)`);
  process.exit(fails ? 1 : 0);
}

/* The backend pushes install progress, game exits and save activity
   through Tauri events rather than returning them, so a mock that
   only answers invoke() never exercises the paths that react to one.
   This gives a suite both halves: a listen() to hand to boot(), and an
   emit() to fire events with. */
function createRuntime() {
  const listeners = [];
  return {
    listeners,
    listen: async (name, fn) => {
      listeners.push({ name, fn });
      return () => {};
    },
    emit: (name, payload) =>
      listeners.filter(l => l.name === name).forEach(l => l.fn({ payload })),
  };
}

/*
 * Loads the app into a DOM with `window.__TAURI__` wired to the given
 * mock, waits for startup to settle, and hands back the window plus
 * anything that went wrong.
 *
 * Pass `html: DEMO_HTML` and no invoke to exercise the published demo,
 * which runs with no Tauri runtime at all.
 */
async function boot({ html = APP_HTML, invoke, listen, confirm, settle = 400 } = {}) {
  const runtime = listen ? null : createRuntime();

  const dom = new JSDOM(fs.readFileSync(html, 'utf8'), {
    runScripts: 'dangerously',
    pretendToBeVisual: true,
    beforeParse(win) {
      if (invoke) {
        win.__TAURI__ = {
          core: { invoke },
          event: { listen: listen || runtime.listen },
        };
      }

      // A blocking dialog would hang the suite; the app uses both.
      win.alert = () => {};
      win.confirm = () => (confirm === undefined ? true : confirm);

      // jsdom implements no layout and no media playback.
      win.HTMLMediaElement.prototype.pause = () => {};
      win.Element.prototype.scrollIntoView = () => {};

      // jsdom ships no Web Audio, and the app plays a tone on every
      // focus move — without this, navigation assertions drown in
      // unrelated environment errors.
      const noop = () => {};
      const param = {
        value: 0, setValueAtTime: noop,
        exponentialRampToValueAtTime: noop, linearRampToValueAtTime: noop,
      };
      const node = () => new Proxy({}, {
        get: (t, k) => (k === 'gain' || k === 'frequency') ? param
                     : (k === 'type' ? 'sine' : () => node()),
      });
      win.AudioContext = function () {
        return new Proxy({}, {
          get: (t, k) => k === 'currentTime' ? 0
                       : (k === 'destination' ? node() : () => node()),
        });
      };

      // With no layout, offsetParent is always null, so the app's
      // "is this element actually visible" checks would all answer
      // no. Back it with the inline display the app itself sets,
      // which is what those checks are really asking about.
      Object.defineProperty(win.HTMLElement.prototype, 'offsetParent', {
        get() {
          for (let el = this; el; el = el.parentElement) {
            if (el.style && el.style.display === 'none') return null;
          }
          return this.parentElement;
        },
      });
    },
  });

  const errors = [];
  dom.window.addEventListener('error', e => errors.push(String(e.error || e.message)));
  await sleep(settle);

  return {
    dom,
    win: dom.window,
    doc: dom.window.document,
    errors,
    emit: runtime ? runtime.emit : null,
  };
}

/* Convenience for the common "open this game's detail view" step. */
async function openGame(doc, title) {
  const card = [...doc.querySelectorAll('.card')].find(c => c.textContent.includes(title));
  if (!card) throw new Error(`no card for ${title}`);
  card.click();
  await sleep(160);
}

module.exports = { APP_HTML, DEMO_HTML, boot, check, sleep, finish, createRuntime, openGame };
