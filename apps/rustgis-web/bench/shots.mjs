// Screenshots of the built web app, for checking a deploy without opening a browser: the Thai
// interface, and the English one with its Thai data fallback.
//
//   node apps/rustgis-web/bench/shots.mjs <app url> <output folder>
//
// Needs Playwright (`npm i playwright`, then `npx playwright install --with-deps chromium`).
import { createRequire } from "node:module";
import { mkdirSync } from "node:fs";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const [base, out] = process.argv.slice(2);
if (!base || !out) {
  console.error("usage: shots.mjs <app url> <output folder>");
  process.exit(2);
}
mkdirSync(out, { recursive: true });

// Software WebGL in a headless runner: enough to draw the app.
const browser = await chromium.launch({ args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"] });
const page = await browser.newPage({ viewport: { width: 1480, height: 900 }, deviceScaleFactor: 1 });
const failed = [];
page.on("requestfailed", (r) => failed.push(r.url()));
page.on("console", (m) => {
  if (m.type() === "error") console.log(`console: ${m.text()}`);
});

for (const [lang, name] of [
  ["th", "web-th.png"],
  ["en", "web-en.png"],
]) {
  await page.goto(`${base}?lang=${lang}`, { waitUntil: "load" });
  // The loading notice goes once the app draws its first frame.
  await page.waitForFunction(() => !document.getElementById("rustgis_loading"), null, { timeout: 180000 });
  // Fonts and the first tiles.
  await page.waitForTimeout(12000);
  await page.screenshot({ path: `${out}/${name}` });
  console.log(`wrote ${out}/${name}`);
}

const fonts = await page.evaluate(async () => {
  const r = await fetch("fonts/NotoSansThai-Regular.ttf");
  return `${r.status} ${(await r.arrayBuffer()).byteLength} bytes`;
});
console.log(`fonts/NotoSansThai-Regular.ttf: ${fonts}`);
if (failed.length) console.log(`failed requests:\n${failed.join("\n")}`);
await browser.close();
