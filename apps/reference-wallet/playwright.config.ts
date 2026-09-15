import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: true,
  workers: 2,
  timeout: 30000,
  use: {
    baseURL: "http://localhost:18081",
    viewport: { width: 390, height: 844 },
    trace: "retain-on-failure",
  },
  webServer: [
    {
      command: "cargo run -p ecashmesh-api",
      cwd: "../..",
      url: "http://127.0.0.1:15000/health",
      timeout: 120000,
      env: {
        ECASHMESH_API_ADDRESS: "127.0.0.1:15000",
        ECASHMESH_WEB_ORIGIN: "http://localhost:18081",
        ROUTING_MODE: "simulator",
      },
    },
    {
      command: "npx expo start --web --localhost --port 18081",
      url: "http://localhost:18081",
      timeout: 120000,
      env: {
        CI: "1",
        BROWSER: "none",
        EXPO_PUBLIC_ECASHMESH_API_URL: "http://127.0.0.1:15000",
        EXPO_PUBLIC_ROUTING_MODE: "simulator",
      },
    },
  ],
});
