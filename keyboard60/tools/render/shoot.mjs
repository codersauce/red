// Usage (from this folder, with the keyboard60 folder served on <port>):
//   node shoot.mjs <port> <outdir> view[@WxH] ...
import { chromium } from 'playwright';

const [port, outdir, ...views] = process.argv.slice(2);
const browser = await chromium.launch({ args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
page.on('console', m => { if (m.type() === 'error') console.error('console:', m.text()); });
page.on('pageerror', e => console.error('pageerror:', e.message));
for (const v of views) {
  const [name, size = '1600x1000'] = v.split('@');
  const [w, h] = size.split('x');
  await page.setViewportSize({ width: +w, height: +h });
  await page.goto(`http://127.0.0.1:${port}/tools/render/render.html?view=${encodeURIComponent(name)}&w=${w}&h=${h}`);
  await page.waitForFunction('window.__done === true', null, { timeout: 180000 });
  const file = `${outdir}/${name.replace(':', '_')}.png`;
  await page.locator('canvas').screenshot({ path: file });
  console.log('wrote', file);
}
await browser.close();
