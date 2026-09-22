// Render pages at desktop and mobile widths so layout is checked by looking, not guessing.
const puppeteer = require('puppeteer');
const path = require('path');
const BASE = process.env.BASE || 'http://127.0.0.1:8099';
const OUT  = process.env.OUT  || '/tmp/shots';
const VIEWS = [
  { name: 'desktop', width: 1440, height: 960,  dsf: 1 },
  { name: 'mobile',  width: 390,  height: 844,  dsf: 2, mobile: true },
];
(async () => {
  const pages = process.argv.slice(2);
  const browser = await puppeteer.launch({
    executablePath: '/usr/bin/google-chrome',
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--font-render-hinting=none'],
  });
  const problems = [];
  for (const rel of pages) {
    for (const v of VIEWS) {
      const pg = await browser.newPage();
      await pg.setCacheEnabled(false);
      await pg.setViewport({ width: v.width, height: v.height, deviceScaleFactor: v.dsf,
                             isMobile: !!v.mobile, hasTouch: !!v.mobile });
      const url = BASE + rel;
      const resp = await pg.goto(url, { waitUntil: 'networkidle2', timeout: 45000 });
      await new Promise(r => setTimeout(r, 400));
      // horizontal overflow is the #1 mobile defect — measure it, don't eyeball it
      const m = await pg.evaluate(() => {
        const de = document.documentElement;
        const over = [...document.querySelectorAll('body *')]
          .filter(e => e.getBoundingClientRect().right > de.clientWidth + 1)
          .slice(0, 5)
          .map(e => e.tagName.toLowerCase() + (e.className ? '.' + String(e.className).split(' ')[0] : ''));
        return { scrollW: de.scrollWidth, clientW: de.clientWidth, over,
                 title: document.title, h1: (document.querySelector('h1')||{}).innerText };
      });
      const slug = rel.replace(/\//g, '_').replace(/^_|_$/g, '') || 'home';
      const file = path.join(OUT, `${slug}.${v.name}.png`);
      await pg.screenshot({ path: file, fullPage: true });
      const bad = m.scrollW > m.clientW + 1;
      if (bad) problems.push(`${rel} [${v.name}] overflows ${m.scrollW}>${m.clientW} via ${m.over.join(', ')}`);
      console.log(`${bad ? 'OVERFLOW' : 'ok      '} ${v.name.padEnd(7)} ${String(resp.status()).padEnd(3)} ${rel}`);
      await pg.close();
    }
  }
  await browser.close();
  if (problems.length) { console.log('\nLAYOUT PROBLEMS:'); problems.forEach(p => console.log('  ' + p)); }
  else console.log('\nno horizontal overflow at either width');
})();
