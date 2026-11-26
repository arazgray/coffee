// Optional regeneration: npm install --no-save lucide@1.80.0
import { copyFileSync, writeFileSync } from 'node:fs';
import * as lucide from 'lucide';

const chrome = [
  'Captions', 'Check', 'ChevronDown', 'Expand', 'FolderOpen',
  'Headphones', 'Info', 'Keyboard', 'Link2', 'ListVideo', 'Minimize', 'Music2',
  'Pause', 'Play', 'Repeat1', 'RotateCcw', 'RotateCw', 'Settings2',
  'SkipBack', 'SkipForward', 'Tv', 'Volume2', 'VolumeX', 'X',
  'ChevronUp', 'Minus', 'Square', 'SquarePlay',
];
function icon(name) {
  const children = lucide[name].map(([tag, attrs]) =>
    `<${tag} ${Object.entries(attrs).map(([key, value]) => `${key}="${value}"`).join(' ')}></${tag}>`,
  ).join('');
  return `<svg aria-hidden="true" viewBox="0 0 24 24" fill="${['Play', 'Pause'].includes(name) ? 'currentColor' : 'none'}" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${children}</svg>`;
}
const artwork = Object.fromEntries(chrome.map(name => [name, icon(name)]));
const shell = artwork.X.slice(0, artwork.X.indexOf('>') + 1);
artwork.LivePhoto = shell + '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="6.2"/><circle cx="12" cy="12" r="2.3" fill="currentColor"/></svg>';
for (const name of Object.keys(artwork)) {
  artwork[name] = artwork[name].replace('<svg ', '<svg xmlns="http://www.w3.org/2000/svg" ');
}
writeFileSync(new URL('../assets/lucide.json', import.meta.url), JSON.stringify(artwork, null, 2) + '\n');
const license = import.meta.resolve('lucide').replace(/\/dist\/.*$/, '/LICENSE');
copyFileSync(new URL(license), new URL('../assets/LUCIDE-LICENSE', import.meta.url));
