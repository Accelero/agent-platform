import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

const certificate = process.env.MANAGER_DEV_TLS_CERT_FILE;
const privateKey = process.env.MANAGER_DEV_TLS_KEY_FILE;
if (Boolean(certificate) !== Boolean(privateKey)) {
  throw new Error("Supply both development TLS files");
}

export default defineConfig({
  plugins: [react()],
  build: { sourcemap: false },
  test: { include: ["src/**/*.test.{ts,tsx}"] },
  server: {
    strictPort: true,
    https:
      certificate && privateKey
        ? {
            cert: readFileSync(certificate),
            key: readFileSync(privateKey),
          }
        : undefined,
    proxy: {
      "/api": {
        target: process.env.MANAGER_DEV_ORIGIN ?? "https://localhost:8443",
      },
      "/auth": {
        target: process.env.MANAGER_DEV_ORIGIN ?? "https://localhost:8443",
      },
    },
  },
});
