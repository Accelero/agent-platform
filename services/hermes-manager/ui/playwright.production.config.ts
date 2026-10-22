import { defineConfig, devices } from "@playwright/test";
export default defineConfig({
  testDir: "./e2e/production",
  timeout: 60_000,
  workers: 1,
  use: {
    ...devices["Desktop Chrome"],
    baseURL: process.env.MANAGER_E2E_ORIGIN,
    ignoreHTTPSErrors: true,
    trace: "off",
  },
});
