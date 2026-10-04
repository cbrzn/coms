import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  workers: 1,
  timeout: 30000,
  use: {
    viewport: { width: 1440, height: 1000 },
    launchOptions: process.env.TMUXOR_CHROMIUM ? { executablePath: process.env.TMUXOR_CHROMIUM } : {},
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
  },
});
