#!/usr/bin/env node
// Render the SVG master and package the same artwork for Windows and the web.
const fs = require('node:fs/promises');
const path = require('node:path');
const sharp = require('sharp');

const root = path.resolve(__dirname, '../..');
const brand = path.join(root, 'assets/brand');
const site = path.join(root, 'site/assets');
const sizes = [16, 20, 24, 32, 40, 48, 64, 128, 192, 256, 512, 1024];

async function png(svg, size) {
  return sharp(Buffer.from(svg), { density: 384 }).resize(size, size).png().toBuffer();
}

function ico(frames) {
  const header = Buffer.alloc(6 + frames.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(frames.length, 4);
  let offset = header.length;
  frames.forEach(({ size, data }, index) => {
    const entry = 6 + index * 16;
    header[entry] = size === 256 ? 0 : size;
    header[entry + 1] = header[entry];
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(data.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += data.length;
  });
  return Buffer.concat([header, ...frames.map(frame => frame.data)]);
}

function artwork(svg) {
  return svg.replace(/<\/?svg[^>]*>/g, '').replace(/<title>.*?<\/title>/s, '');
}

function preview(svg) {
  const icon = artwork(svg);
  const use = (x, y, size) => `<g transform="translate(${x} ${y}) scale(${size / 512})">${icon}</g>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="960" viewBox="0 0 1440 960">
  <rect width="1440" height="960" fill="#F4F3E9"/>
  <g font-family="Segoe UI, Arial, sans-serif" fill="#20291F">
    <text x="80" y="70" font-size="14" letter-spacing="3">DISCODEX / BRAND IDENTITY</text>
    <text x="1360" y="70" font-size="14" text-anchor="end">01 / PRESENCE</text>
    ${use(92, 157, 308)}
    <text x="452" y="329" font-size="110" font-weight="600" letter-spacing="-5">discodex<tspan fill="#7B963C">.</tspan></text>
    <text x="458" y="382" font-size="23" fill="#646C5C">Your activity, in good company.</text>
    <path d="M80 535H1360" stroke="#CFD4C5"/>
    <text x="80" y="587" font-size="13" letter-spacing="2">BUILT TO BE RECOGNIZED</text>
    <rect x="80" y="620" width="610" height="222" rx="18" fill="#E6E9DC"/>
    <rect x="718" y="620" width="642" height="222" rx="18" fill="#20291F"/>
    ${[96, 64, 48, 32, 24, 16].map((size, i) => use([112, 240, 340, 430, 510, 580][i], 676 + (96 - size) / 2, size)).join('')}
    ${[96, 64, 48, 32, 24, 16].map((size, i) => use(754 + i * 96, 676 + (96 - size) / 2, size)).join('')}
    <text x="112" y="816" font-size="12" fill="#646C5C">APP / TRAY / FAVICON</text>
    <text x="754" y="816" font-size="12" fill="#A9B29F">DARK BACKGROUNDS</text>
    <circle cx="92" cy="900" r="10" fill="#20291F"/>
    <text x="114" y="905" font-size="13">Forest charcoal / #20291F</text>
    <circle cx="450" cy="900" r="10" fill="#F4F3E9" stroke="#BCC4B0"/>
    <text x="472" y="905" font-size="13">Warm ivory / #F4F3E9</text>
    <circle cx="800" cy="900" r="10" fill="#C5F277"/>
    <text x="822" y="905" font-size="13">Live lime / #C5F277</text>
  </g>
</svg>`;
}

function social(svg) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630">
  <rect width="1200" height="630" fill="#F4F3E9"/>
  <g transform="translate(72 145) scale(.59)">${artwork(svg)}</g>
  <g font-family="Segoe UI, Arial, sans-serif" fill="#20291F">
    <text x="425" y="286" font-size="88" font-weight="600" letter-spacing="-4">discodex<tspan fill="#7B963C">.</tspan></text>
    <text x="430" y="344" font-size="28">Your work. In good company.</text>
    <text x="430" y="392" font-size="20" fill="#646C5C">AI coding activity, live in Discord.</text>
    <path d="M88 520H1112" stroke="#CFD4C5"/>
    <text x="88" y="562" font-size="16" fill="#646C5C">WINDOWS / OPEN SOURCE / PRIVATE BY DESIGN</text>
  </g>
</svg>`;
}

async function main() {
  const svg = await fs.readFile(path.join(brand, 'discodex-icon.svg'), 'utf8');
  const bare = svg.replace(/\s*<rect id="plate"[^>]*\/>/, '').replace('0 0 512 512', '96 90 304 312');
  const mono = bare.replaceAll('#C5F277', '#F4F3E9');
  await fs.writeFile(path.join(brand, 'discodex-mark.svg'), bare.replaceAll('#F4F3E9', '#20291F'));
  await fs.writeFile(path.join(brand, 'discodex-mono-dark.svg'), mono.replaceAll('#F4F3E9', '#20291F'));
  await fs.writeFile(path.join(brand, 'discodex-mono-light.svg'), mono);
  await fs.mkdir(site, { recursive: true });
  const images = new Map();
  for (const size of sizes) {
    const data = await png(svg, size);
    images.set(size, data);
    await fs.writeFile(path.join(brand, `discodex-${size}.png`), data);
  }
  const iconSizes = [16, 20, 24, 32, 40, 48, 64, 128, 256];
  await fs.writeFile(path.join(root, 'assets/discodex.ico'), ico(iconSizes.map(size => ({ size, data: images.get(size) }))));
  await fs.writeFile(path.join(site, 'favicon.ico'), ico([16, 32, 48, 64].map(size => ({ size, data: images.get(size) }))));
  for (const size of [64, 192, 256]) {
    await fs.writeFile(path.join(site, `discodex-${size}.png`), images.get(size));
  }
  await fs.writeFile(path.join(site, 'discodex.svg'), svg);
  await fs.writeFile(path.join(brand, 'preview.svg'), preview(svg));
  await sharp(Buffer.from(preview(svg))).png().toFile(path.join(brand, 'preview.png'));
  await sharp(Buffer.from(social(svg))).png().toFile(path.join(site, 'og-card.png'));
  console.log(`Built ${sizes.length} PNG sizes, SVG variants, Windows ICO, favicon, preview, and social card.`);
}

main().catch(error => { console.error(error); process.exitCode = 1; });
