import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import sharp from 'sharp';

const directory = path.dirname(fileURLToPath(import.meta.url));
const official = await readFile(path.join(directory, 'antigravity-official.svg'), 'utf8');
const paths = [...official.matchAll(/<path\b[^>]*\bd="([^"]+)"[^>]*>/g)].map((match) => match[1]);
if (paths.length !== 1) throw new Error('Unexpected official Antigravity mark structure');

// 官方几何在 180x180 中大约占 x:[22..158] y:[28..152]，缩放 4.35 倍后约 590x540，居中落在 1024x1024 画布内。
const mark = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="ice" x1="0" y1="0" x2="0" y2="1">
      <stop stop-color="#F4FBFF"/>
      <stop offset="1" stop-color="#CFE6FF"/>
    </linearGradient>
  </defs>
  <g transform="translate(120 120) scale(4.35)">
    <path d="${paths[0]}" fill="url(#ice)" stroke="#1D365E" stroke-width="5.2" stroke-linejoin="round" paint-order="stroke"/>
    <path d="${paths[0]}" fill="url(#ice)" stroke="#5BA8FF" stroke-width="2.1" stroke-linejoin="round" paint-order="stroke"/>
  </g>
</svg>
`;
await writeFile(path.join(directory, 'mark-styled.svg'), mark);
await sharp(Buffer.from(mark)).png().toFile(path.join(directory, 'mark-generated.png'));
console.log('Prepared Antigravity foreground mark from official SVG.');
