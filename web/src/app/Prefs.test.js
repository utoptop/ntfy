import { describe, expect, it, vi } from "vitest";
import { Prefs, DATE_FORMAT, TIME_FORMAT, PREF_DEFAULTS } from "./Prefs";

// The module instantiates `new Prefs(db())` at import; stub db() so that doesn't touch IndexedDB.
vi.mock("./db", () => ({ default: () => ({}) }));

// Minimal fake of the Dexie `prefs` table: get() returns a stored {key, value} or undefined.
const fakeDb = (stored) => ({
  prefs: {
    get: async (key) => (key in stored ? { key, value: stored[key] } : undefined),
    put: async () => {},
  },
});

describe("Prefs date/time format validation", () => {
  it("returns the stored value when it is a known format", async () => {
    const prefs = new Prefs(fakeDb({ dateFormat: DATE_FORMAT.ISO8601, timeFormat: TIME_FORMAT.H24 }));
    expect(await prefs.dateFormat()).toBe(DATE_FORMAT.ISO8601);
    expect(await prefs.timeFormat()).toBe(TIME_FORMAT.H24);
  });

  it("falls back to the default for an unknown stored value (e.g. synced from a newer client)", async () => {
    const prefs = new Prefs(fakeDb({ dateFormat: "newer-client-format", timeFormat: "36h" }));
    expect(await prefs.dateFormat()).toBe(PREF_DEFAULTS.dateFormat);
    expect(await prefs.timeFormat()).toBe(PREF_DEFAULTS.timeFormat);
  });

  it("falls back to the default when unset", async () => {
    const prefs = new Prefs(fakeDb({}));
    expect(await prefs.dateFormat()).toBe(PREF_DEFAULTS.dateFormat);
    expect(await prefs.timeFormat()).toBe(PREF_DEFAULTS.timeFormat);
  });
});
