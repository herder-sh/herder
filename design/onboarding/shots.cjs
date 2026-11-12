// Renders every step of the prototype, iPhone and Mac frames separately, to PNG.
//   npm i playwright   (any version; it drives the system Chromium)
//   CHROMIUM=/usr/bin/chromium node shots.cjs <out-dir> [f.s ...]
// With step ids (e.g. 1.5 2.3) it renders only those.
const path = require('path');
const { chromium } = require('playwright');
const out = process.argv[2] || 'screenshots';
const only = process.argv.slice(3);
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined });
  const url = 'file://' + path.join(__dirname, 'index.html');
  for (const [dev, scale] of [['phone', 2], ['mac', 1.5]]) {
    const page = await browser.newPage({ viewport: { width: 1500, height: 1200 }, deviceScaleFactor: scale });
    await page.goto(url);
    const flows = await page.evaluate(() => FLOWS.map(f => f.steps.length));
    for (let f = 0; f < flows.length; f++) for (let s = 0; s < flows[f]; s++) {
      const id = `${f + 1}.${s + 1}`;
      if (only.length && !only.includes(id)) continue;
      await page.evaluate(h => { location.hash = h; }, `f=${f + 1}&s=${s + 1}&d=${dev}`);
      await page.waitForTimeout(120);
      await page.locator(dev === 'phone' ? '.phone' : '.mac').first()
        .screenshot({ path: path.join(out, `${id}-${dev === 'phone' ? 'iphone' : 'mac'}.png`), animations: 'disabled' });
    }
    await page.close();
  }
  await browser.close();
})();
