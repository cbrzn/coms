import { test, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, rmSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const binary = fileURLToPath(new URL('../../target/debug/tmuxor', import.meta.url));
const agentScript = fileURLToPath(new URL('./agent.py', import.meta.url));
const quote = (s) => `'${s.replaceAll("'", "'\"'\"'")}'`;
let root, repo, home, env, url, token, origin;
const tmux = (...args) => execFileSync('tmux', args, { env, encoding: 'utf8' }).trim();
const receipts = (role) => {
  try { return readFileSync(join(home, `received-${role}.jsonl`), 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse); }
  catch { return []; }
};
function queue(from, to, text) {
  execFileSync(binary, ['relay-stop'], { env: { ...env, TMUXOR_HOME: home, TMUXOR_ROLE: from }, input: JSON.stringify({ last_assistant_message: `${to ? `[TO: ${to}]\n` : ''}${text}` }) });
  return readdirSync(join(home, 'queue')).sort().at(-1);
}
async function open(page) {
  await page.goto(url);
  await expect(page.locator('#connection-label')).toHaveText('Connected');
  await expect(page.locator('.terminal-connection').first()).toHaveText('Live');
}

test.describe.configure({ mode: 'serial' });
test.beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), 'tmuxor-ui-'));
  repo = join(root, 'repo');
  home = join(repo, '.tmuxor');
  mkdirSync(join(repo, 'agora'), { recursive: true });
  mkdirSync(join(root, 'bin'));
  execFileSync('git', ['init', '-q', repo]);
  execFileSync('git', ['-C', repo, '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '--allow-empty', '-qm', 'fixture']);
  writeFileSync(join(repo, 'agora/team.txt'), 'claude specifier\nopencode builder\nclaude tester\n');
  const realTmux = execFileSync('which', ['tmux'], { encoding: 'utf8' }).trim();
  writeFileSync(join(root, 'bin/tmux'), `#!/bin/sh\nexec ${quote(realTmux)} -S ${quote(join(root, 'tmux.sock'))} "$@"\n`);
  chmodSync(join(root, 'bin/tmux'), 0o755);
  for (const name of ['claude', 'opencode']) {
    writeFileSync(join(root, 'bin', name), `#!/bin/sh\nexec python3 ${quote(agentScript)}\n`);
    chmodSync(join(root, 'bin', name), 0o755);
  }
  env = { ...process.env, PATH: `${join(root, 'bin')}:${process.env.PATH}`, TMUX: '' };
  const output = execFileSync(binary, [repo, '--ui', '--no-view'], { env, encoding: 'utf8', timeout: 20000 });
  url = output.match(/dashboard: (http:\/\/[^\s]+)/)?.[1];
  if (!url) throw new Error(output);
  token = new URLSearchParams(new URL(url).hash.slice(1)).get('token');
  origin = new URL(url).origin;
});
test.afterAll(() => {
  if (env) { try { tmux('kill-server'); } catch { /* already stopped */ } }
  if (root) rmSync(root, { recursive: true, force: true });
});

test('connects to real terminals, accepts typing, and preserves sessions after refresh', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await open(page);
  await expect(page.locator('#agents .agent')).toHaveCount(3);
  const pid = tmux('display-message', '-p', '-t', 'tmuxor-specifier', '#{pane_pid}');
  const width = Number(tmux('display-message', '-p', '-t', 'tmuxor-specifier', '#{pane_width}'));
  await page.locator('.xterm-helper-textarea').first().focus();
  await page.keyboard.type('Hello from the browser');
  await page.keyboard.press('Enter');
  await expect.poll(() => receipts('specifier').at(-1)).toBe('Hello from the browser');
  await page.getByRole('button', { name: 'Split', exact: false }).click();
  await expect(page.locator('.terminal-panel:visible')).toHaveCount(3);
  await expect.poll(() => Number(tmux('display-message', '-p', '-t', 'tmuxor-specifier', '#{pane_width}'))).toBeLessThan(width);
  await page.reload();
  await expect(page.locator('.terminal-panel:visible')).toHaveCount(3);
  await expect(page.locator('.terminal-connection').first()).toHaveText('Live');
  await expect.poll(() => tmux('list-clients', '-F', '#{client_pid}').split('\n').length).toBe(3);
  expect(tmux('display-message', '-p', '-t', 'tmuxor-specifier', '#{pane_pid}')).toBe(pid);
  expect(errors).toEqual([]);
});

test('operator messages preserve multiline input and relay headers', async ({ page }) => {
  await open(page);
  await page.locator('#agents .agent').filter({ hasText: 'builder' }).click();
  await page.locator('#operator-text').fill('Implement the change.\n\nKeep the tests passing.');
  await page.locator('#operator-send').click();
  await expect.poll(() => receipts('builder').at(-1)).toBe('[FROM OPERATOR]\n\nImplement the change.\n\nKeep the tests passing.');
});

test('reviews, edits, and retargets a handoff exactly once', async ({ page, request }) => {
  const before = receipts('tester').length;
  const id = queue('builder', 'specifier', 'Implementation is ready.\nPlease check it.');
  await open(page);
  await expect(page.locator('#review-text')).toHaveValue('Implementation is ready.\nPlease check it.');
  expect(receipts('tester')).toHaveLength(before);
  await page.locator('#review-text').fill('Review the retry implementation.\nCheck timeout handling.');
  await page.locator('#review-to').selectOption('tester');
  await page.locator('#send-message').click();
  await expect.poll(() => receipts('tester').at(-1)).toBe('[FROM BUILDER]\n\nReview the retry implementation.\nCheck timeout handling.');
  await expect(page.locator('#inbox-count')).toHaveText('0');
  const duplicate = await request.post(`${origin}/api/messages/${id}`, { headers: { Authorization: `Bearer ${token}` }, data: { action: 'send', to: 'tester', text: 'duplicate' } });
  expect(duplicate.status()).toBe(409);
  expect(receipts('tester')).toHaveLength(before + 1);
  queue('specifier', 'tester', 'One more check.');
  await expect(page.locator('#review-text')).toHaveValue('One more check.');
  await expect(page.locator('#send-message')).toBeDisabled();
  await page.locator('#busy-confirm').check();
  await page.locator('#send-message').click();
  await expect.poll(() => receipts('tester').at(-1)).toBe('[FROM SPECIFIER]\n\nOne more check.');
  await page.locator('#activity-tab').click();
  await expect(page.locator('#activity-list')).toContainText('Check timeout handling.');
});

test('keeps human messages at the broker and requires a recipient for unrouted messages', async ({ page }) => {
  queue('tester', 'human', 'Which retry limit should we use?');
  await open(page);
  await expect(page.locator('#send-message')).toHaveText('Acknowledge ✓');
  await page.locator('#send-message').click();
  await expect(page.locator('#inbox-count')).toHaveText('0');
  queue('specifier', '', '<img src=x onerror="window.injected=true"> needs a destination');
  await expect(page.locator('#review-text')).toHaveValue('<img src=x onerror="window.injected=true"> needs a destination');
  await expect(page.locator('#send-message')).toBeDisabled();
  expect(await page.evaluate(() => window.injected)).toBeUndefined();
  await page.locator('#drop-message').click();
  await expect(page.locator('#inbox-count')).toHaveText('0');
});

test('protects the local API and supports a narrow screen', async ({ page, request }) => {
  expect((await request.get(`${origin}/api/state`)).status()).toBe(401);
  expect((await request.get(`${origin}/api/state`, { headers: { Authorization: `Bearer ${token}`, Origin: 'https://other.example' } })).status()).toBe(403);
  await page.setViewportSize({ width: 390, height: 844 });
  await open(page);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.locator('#broker').scrollIntoViewIfNeeded();
  await expect(page.locator('#inbox-empty')).toBeVisible();
});

test('shows the team and a pending review together', async ({ page }) => {
  queue('specifier', 'builder', 'Add exponential backoff to the HTTP client.\n\n• Retry transient failures up to 3 times.\n• Preserve the current timeout behavior.\n• Hand the implementation to tester when ready.');
  await open(page);
  await page.locator('#agents .agent').filter({ hasText: 'builder' }).click();
  await expect(page.locator('#review-text')).toHaveValue(/Add exponential backoff/);
  await expect(page.locator('#inbox-count')).toHaveText('1');
  await page.screenshot({ path: resolve('test-results/dashboard.png'), fullPage: true });
});
