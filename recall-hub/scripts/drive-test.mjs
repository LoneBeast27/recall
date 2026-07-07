// E2E drive of the live hub window over CDP (WebView2 remote debugging).
// Verifies: live daemon data renders, the REAL Save button produces a clip
// (toast), the HDR page renders and the profile switch round-trips.
import puppeteer from 'puppeteer-core';

const shot = (page, name) =>
  page.screenshot({ path: `L:/Docker Speed Drive/captures/debug/${name}.png` });

const browser = await puppeteer.connect({
  browserURL: 'http://127.0.0.1:9223',
  defaultViewport: null,
});
const pages = await browser.pages();
const page = pages.find((p) => p.url().startsWith('http://localhost:1420'));
if (!page) throw new Error('hub page not found');

// 0. Ensure we start on the Clips page.
await page.click('a[href="/"]');
await page.waitForSelector('.gauge', { timeout: 10000 });

// 1. Live data present?
await page.waitForFunction(
  () => document.body.innerText.includes('h264_nvenc'),
  { timeout: 20000 }
);
const gauge = await page.evaluate(() => {
  const label = document.querySelector('.gauge-label')?.innerText ?? '';
  const sub = document.querySelector('.gauge-sub')?.innerText ?? '';
  const conn = document.querySelector('.conn')?.innerText ?? '';
  return { label, sub, conn };
});
console.log('LIVE:', JSON.stringify(gauge));
await shot(page, 'hub-clips-live');

// 2. Click the real Save 60s button -> expect the ok toast.
const buttons = await page.$$('button.save');
await buttons[0].click();
await page.waitForSelector('.toast.ok', { timeout: 20000 });
const toast = await page.$eval('.toast.ok', (el) => el.innerText);
console.log('TOAST:', toast);
await shot(page, 'hub-clips-saved');

// 3. HDR page (innerText is CSS-uppercased -> match case-insensitively).
await page.click('a[href="/hdr"]');
await page.waitForFunction(
  () => /sdr-in-hdr profile/i.test(document.body.innerText),
  { timeout: 10000 }
);
await shot(page, 'hub-hdr');

// 4. Profile switch round-trip (sets punchy — already the intended default).
const acts = await page.$$('button.act');
for (const a of acts) {
  const t = await a.evaluate((el) => el.innerText);
  if (t.startsWith('Punchy')) {
    await a.click();
    break;
  }
}
await page.waitForSelector('.note', { timeout: 10000 });
const note = await page.$eval('.note', (el) => el.innerText);
console.log('PROFILE:', note);
await shot(page, 'hub-hdr-profile');

await browser.disconnect();
console.log('DRIVE TEST COMPLETE');
