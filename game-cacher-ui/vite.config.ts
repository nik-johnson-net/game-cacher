import path from "path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type PluginOption, type ProxyOptions } from "vite";

// The Rust app (game-cacher admin listener) listens here during development.
const BACKEND = "http://127.0.0.1:8080";

const proxy: Record<string, ProxyOptions> = Object.fromEntries(
  ["/api", "/metrics", "/healthz"].map((prefix) => [prefix, { target: BACKEND, changeOrigin: true }]),
);

// https://vite.dev/config/
// `npm run dev:mock` (`vite --mode mock`) serves fixture data from mock/ instead of
// proxying to the backend. The mock is a dev-server plugin only: it never reaches
// the production bundle.
export default defineConfig(async ({ command, mode }) => {
  const useMock = command === "serve" && mode === "mock";
  const plugins: PluginOption[] = [react(), tailwindcss()];
  if (useMock) {
    const { mockApi } = await import("./mock/mock-api");
    plugins.push(mockApi());
  }

  return {
    plugins,
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "./src"),
      },
    },
    server: {
      proxy: useMock ? undefined : proxy,
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
      rollupOptions: {
        output: {
          // Long-lived vendor chunk (React, router, Radix) apart from app code.
          manualChunks(id: string) {
            if (/[\\/]node_modules[\\/](react|react-dom|scheduler|react-router|@radix-ui)[\\/]/.test(id)) return "vendor";
          },
        },
      },
    },
  };
});
