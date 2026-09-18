/*
 * Renders the app in headless Chromium against a representative mock
 * library and writes PNGs, for looking at a change rather than
 * asserting on it.
 *
 *   npm install --no-save playwright && npx playwright install chromium
 *   node screenshots.js [output-dir]
 *
 * Set CHROMIUM_PATH to use a browser you already have instead of the
 * one Playwright downloads.
 *
 * Deliberately not part of `npm test`: it needs a browser download,
 * and a screenshot is evidence for a person, not a passing condition.
 */
const path = require('path');

const OUT = process.argv[2] || '.';
const APP = 'file://' + path.join(__dirname, '..', 'frontend', 'index.html');

const game = (id, title, platform, year, desc, size) => ({
  id, title, platform, release_year: year, description: desc,
  files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: size }],
  screenshots: [], cover: null, trailer: null,
});

(async () => {
  let chromium;
  try {
    ({ chromium } = require('playwright'));
  } catch {
    console.error('playwright is not installed here — see the header of this file');
    process.exit(2);
  }

  const browser = await chromium.launch(
    process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2 });
  const page = await ctx.newPage();

  await page.addInitScript(() => {
    const g = (id, title, platform, year, size) => ({
      id, title, platform, release_year: year, description: 'A game.',
      files: [{ filename: 'game.bin', format: 'bin', needs_conversion: false, size_bytes: size }],
      screenshots: [], cover: null, trailer: null });
    const library = [
      g('PC/Hollow Meridian', 'Hollow Meridian', 'PC', 2021, 4e9),
      g('Switch/Dorfromantik', 'Dorfromantik', 'Switch', 2022, 9e8),
      g('PS1/Static Choir', 'Static Choir', 'PS1', 1999, 6e8),
      g('PS2/Wraith Hollow', 'Wraith Hollow', 'PS2', 2004, 3e9),
      g('PC/Tidebreaker', 'Tidebreaker', 'PC', 2020, 9e9),
    ];
    const installed = {
      'PC/Hollow Meridian': { status: 'installed', local_dir: '/games/PC/Hollow Meridian' },
      'Switch/Dorfromantik': { status: 'installed', local_dir: '/games/Switch/Dorfromantik' },
    };
    const settings = {
      server_base: 'http://library:8420', install_root: '/games', sound_enabled: false,
      emulators: { PS1:{command:'duckstation-qt',args_prefix:[],version_flag:'-version'},
                   PS2:{command:'pcsx2-qt',args_prefix:[],version_flag:'--version'},
                   PC:{command:'wine',args_prefix:[],version_flag:'--version'},
                   Switch:{command:'eden',args_prefix:[],version_flag:'--version'} },
      launch_overrides: {}, prefix_root: '',
      save_sync: { enabled: true, device_name: 'laptop', switch_data_dir: null, title_ids: {} },
    };
    window.__TAURI__ = {
      core: { invoke: async (cmd) => {
        if (cmd === 'get_settings') return settings;
        if (cmd === 'get_cached_library') return library.filter(x => installed[x.id]);
        if (cmd === 'fetch_library') return library;
        if (cmd === 'get_install_states') return installed;
        return null; } },
      event: { listen: async () => () => {} },
    };
  });

  await page.goto(APP);
  await page.waitForTimeout(900);
  await page.screenshot({ path: path.join(OUT, 'grid.png') });

  await page.locator('.card').first().click();
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(OUT, 'detail.png') });

  await page.locator('.nav-item[data-type="settings"]').click();
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(OUT, 'settings.png') });

  await browser.close();
  console.log(`wrote grid.png, detail.png and settings.png to ${path.resolve(OUT)}`);
})();
