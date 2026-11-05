/* eslint-disable import/no-extraneous-dependencies */
import { defineConfig } from "vitest/config";

// Standalone config (separate from vite.config.js) so the PWA plugin isn't loaded during tests.
// Tests run in the default "node" environment -- see src/test/setup.js for the minimal window stub
// that lets the pure-logic modules import without jsdom.
export default defineConfig({
  test: {
    environment: "node",
    setupFiles: ["./src/test/setup.js"],
    include: ["src/**/*.test.{js,jsx}"],
    globals: false, // explicit `import { describe, it, expect } from "vitest"` -> no eslint env change
  },
});
