import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid({ hot: !process.env.VITEST })],
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:27863", ws: true },
      "/preview": "http://127.0.0.1:27863",
    },
  },
  build: { target: "es2022" },
  test: {
    environment: "jsdom",
  },
});
