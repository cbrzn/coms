import { copyFile, mkdir, writeFile } from 'node:fs/promises';

await mkdir(new URL('vendor/', import.meta.url), { recursive: true });
for (const [source, destination] of [
  ['@xterm/xterm/lib/xterm.js', 'xterm.js'],
  ['@xterm/xterm/css/xterm.css', 'xterm.css'],
  ['@xterm/xterm/LICENSE', 'LICENSE-xterm'],
  ['@xterm/addon-fit/lib/addon-fit.js', 'addon-fit.js'],
  ['@xterm/addon-fit/LICENSE', 'LICENSE-addon-fit'],
]) {
  await copyFile(new URL(`node_modules/${source}`, import.meta.url), new URL(`vendor/${destination}`, import.meta.url));
}
await writeFile(new URL('vendor/README.md', import.meta.url),
  'Bundled browser assets: @xterm/xterm 6.0.0 and @xterm/addon-fit 0.11.0 (MIT).\n\nRegenerate with `npm ci && npm run vendor` in `web/`. No CDN or npm installation is needed at runtime.\n');
