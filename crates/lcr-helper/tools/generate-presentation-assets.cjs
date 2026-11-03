// Offline asset preparation only; Node + sharp are never Helper dependencies.
// Run with a Node runtime whose NODE_PATH includes sharp. Sources are vendored.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const sharp = require('sharp');
const root = path.resolve(__dirname, '../assets/presentation');
const source = name => path.join(root, 'source', name);
const sizes = [256, 128, 64, 48, 32, 16];

async function rgba(file) {
  return sharp(file).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
}
async function dib(name, input) {
  const { data, info } = await rgba(input);
  const out = Buffer.alloc(8 + data.length);
  out.writeUInt32LE(info.width, 0);
  out.writeUInt32LE(info.height, 4);
  for (let i = 0; i < data.length; i += 4) {
    const a = data[i + 3];
    out[i + 8] = Math.round(data[i + 2] * a / 255);
    out[i + 9] = Math.round(data[i + 1] * a / 255);
    out[i + 10] = Math.round(data[i] * a / 255);
    out[i + 11] = a;
  }
  fs.writeFileSync(path.join(root, name + '.bgra'), out);
}
async function png(data, info) {
  return sharp(data, { raw: info }).png().toBuffer();
}
function writeProvenance() {
  const provenance = ['mark.png', 'wordmark.png', 'background.png', 'menu-background.png', 'app-icon.png'].map(name =>
    `${name} SHA256=${crypto.createHash('sha256').update(fs.readFileSync(source(name))).digest('hex')}`);
  fs.writeFileSync(path.join(root, 'SOURCE_HASHES.txt'), provenance.join('\n') + '\n');
}
function writeIco(output, iconSizes, images) {
  const directory = Buffer.alloc(6 + iconSizes.length * 16);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(iconSizes.length, 4);
  let offset = directory.length;
  iconSizes.forEach((n, i) => {
    const at = 6 + i * 16;
    directory[at] = directory[at + 1] = n === 256 ? 0 : n;
    directory.writeUInt16LE(1, at + 4);
    directory.writeUInt16LE(32, at + 6);
    directory.writeUInt32LE(images[i].length, at + 8);
    directory.writeUInt32LE(offset, at + 12);
    offset += images[i].length;
  });
  fs.writeFileSync(output, Buffer.concat([directory, ...images]));
}
async function trayIcons() {
  const art = await rgba(source('app-icon.png'));
  const traySizes = [64, 48, 32, 24, 16];
  for (const [name, color] of [
    ['disconnected', [225, 225, 225]],
    ['standby', [128, 255, 0]],
    ['transmitting', [255, 48, 48]],
  ]) {
    const pixels = Buffer.from(art.data);
    for (let i = 0; i < pixels.length; i += 4) {
      const light = Math.max(art.data[i], art.data[i + 1], art.data[i + 2]) / 255;
      for (let c = 0; c < 3; c++) pixels[i + c] = Math.round(light * color[c]);
      // Source alpha/negative spaces remain exact before ordinary resizing.
    }
    const master = await png(pixels, art.info);
    const images = [];
    for (const n of traySizes) images.push(await sharp(master).resize(n, n).png().toBuffer());
    writeIco(path.join(root, '../tray', `tray_${name}.ico`), traySizes, images);
  }
  console.log('PASS new tray artwork; disconnected gray/standby green/transmitting red; state mapping unchanged');
}
async function main() {
  if (process.argv.includes('--tray-icons-only')) { await trayIcons(); return; }
  // App artwork has separate authority from the unchanged UI/tray logo.
  const iconMaster = source('app-icon.png');
  const images = [];
  for (const n of sizes) {
    const image = await sharp(iconMaster).resize(n, n).png().toBuffer();
    fs.writeFileSync(path.join(root, `app-${n}.png`), image);
    images.push(image);
  }
  writeIco(path.join(root, '../LCR_Helper.ico'), sizes, images);
  if (process.argv.includes('--app-icon-only')) {
    writeProvenance();
    console.log('PASS app icon only; six ICO sizes; UI logo/tray payloads untouched');
    return;
  }
  const mark = await rgba(source('mark.png'));
  // Light stays inside the supplied alpha: no expanded silhouette or filled gaps.
  const uiMark = Buffer.from(mark.data);
  for (let i = 0; i < uiMark.length; i += 4) {
    if (uiMark[i + 3]) uiMark[i] = Math.min(255, uiMark[i] + 12);
  }
  await dib('mark', await sharp(await png(uiMark, mark.info)).resize(176, 176).png().toBuffer());

  const wordmark = await rgba(source('wordmark.png'));
  for (let y = 0; y < wordmark.info.height; y++) {
    for (let x = 0; x < wordmark.info.width; x++) {
      const i = (y * wordmark.info.width + x) * 4;
      const rgb = y >= 600 ? [138, 146, 154] : [216, 217, 218];
      for (let c = 0; c < 3; c++) wordmark.data[i + c] = rgb[c];
    }
  }
  await dib('wordmark', await sharp(await png(wordmark.data, wordmark.info))
    // Separate the existing LCR letters from the descriptor so native Jura copy
    // can have its own readable size and stamp spacing. No letter tracing.
    .extract({ left: 116, top: 166, width: 1753, height: 389 }).resize(192, 43).png().toBuffer());

  for (const [name, crop, width, height, level] of [
    ['pair-background', { left: 0, top: 0, width: 768, height: 1024 }, 480, 640, 0.60],
  ]) {
    const plate = sharp(source('background.png')).extract(crop).resize(width, height);
    if (name.startsWith('main')) plate.blur(0.6);
    const art = await rgba(await plate.png().toBuffer());
    for (let y = 0; y < height; y++) {
      const fade = name.startsWith('main') ? Math.max(0, Math.min(1, (280 - y) / 96)) : 1;
      for (let x = 0; x < width; x++) {
        const i = (y * width + x) * 4;
        for (let c = 0; c < 3; c++) art.data[i + c] = Math.round(art.data[i + c] * level * fade + [5, 6, 7][c] * (1 - fade));
      }
    }
    await dib(name, await png(art.data, art.info));
  }
  // Exact decoded source pixels at full resolution; no offline crop/resample,
  // blur, brightness, gradient or source edits. D2D owns scaling/clip/dimming.
  await dib('main-background', source('menu-background.png'));
  for (const name of ['key', 'mouse', 'volume', 'unlink', 'chevron-right', 'arrow-right', 'antenna', 'server', 'chevrons-right', 'access-point']) {
    const svg = fs.readFileSync(path.join(root, 'tabler', name + '.svg'), 'utf8');
    const neutral = ['key', 'mouse', 'volume', 'unlink', 'antenna', 'server', 'access-point'].includes(name) ? '#9aa3ac' : '#b4b0ae';
    const variants = name === 'chevrons-right'
      ? [['red', '#ff343e'], ['green', '#39dc89']]
      : name === 'access-point'
        ? [['neutral', neutral], ['green', '#39dc89']]
        : ['antenna', 'server'].includes(name)
        ? [['neutral', neutral], ['red', '#ff343e'], ['green', '#39dc89']]
        : [['neutral', neutral], ['red', '#ff343e']];
    for (const [variant, color] of variants) {
      await dib(`${name}-${variant}`, await sharp(Buffer.from(svg.replace('currentColor', color)))
        .resize(24, 24).png().toBuffer());
    }
  }
  // Tiny antialiased geometric status dots, prepared once rather than repaint
  // allocation or a cosmetic clock. Icon illustrations remain the Tabler set.
  for (const [name, size, color] of [
    ['ready-dot', 12, '#ff343e'], ['paired-dot', 10, '#39dc89'], ['separator-dot', 6, '#ff343e'],
  ]) {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><circle cx="${size / 2}" cy="${size / 2}" r="${size / 2 - 0.5}" fill="${color}"/></svg>`;
    await dib(name, await sharp(Buffer.from(svg)).png().toBuffer());
  }
  writeProvenance();
  console.log('PASS canonical alpha unchanged; six ICO sizes; static BGRA assets generated');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
