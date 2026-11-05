import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import session from "./Session";
import prefs from "./Prefs";
import subscriptionManager from "./SubscriptionManager";
import accountApi from "./AccountApi";

// AccountApi.js leans on several singletons; mock them so we can drive token() and assert the
// side effects of sync(), and so importing it doesn't drag in the component/Dexie/i18n trees.
// (vi.mock is hoisted above the imports by vitest, so those imports receive the mocks.)
vi.mock("./Session", () => ({
  default: { token: vi.fn(() => "test-token"), setLastExtendedAtAsync: vi.fn(), resetAndRedirect: vi.fn() },
}));
vi.mock("./SubscriptionManager", () => ({ default: { syncFromRemote: vi.fn() } }));
vi.mock("./Prefs", () => ({
  default: { setSound: vi.fn(), setDeleteAfter: vi.fn(), setMinPriority: vi.fn(), setDateFormat: vi.fn(), setTimeFormat: vi.fn() },
  THEME: { DARK: "dark", LIGHT: "light", SYSTEM: "system" },
}));
vi.mock("i18next", () => ({ default: { changeLanguage: vi.fn() } }));
vi.mock("../components/routes", () => ({ default: { login: "/login" } }));

let fetchMock;

// fetchOrThrow treats anything other than HTTP 200 as an error, so a fake response just needs a
// status and a json() method.
const ok = (body = {}) => ({ status: 200, json: async () => body });

beforeEach(() => {
  vi.clearAllMocks();
  vi.spyOn(console, "log").mockImplementation(() => {});
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  session.token.mockReturnValue("test-token");
  accountApi.tiers = null; // reset the billing-tiers cache between tests
  accountApi.listener = null;
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("AccountApi.login", () => {
  it("POSTs basic auth to the login URL and returns the token and canonical username", async () => {
    // The typed identifier is an email; the login endpoint returns the canonical username in one
    // request, so login() surfaces it (callers store it) rather than echoing what was typed.
    fetchMock.mockResolvedValue(ok({ token: "tk_returned", username: "phil" }));
    const result = await accountApi.login({ username: "phil@example.com", password: "secret" });

    expect(result).toEqual({ token: "tk_returned", username: "phil" });
    expect(fetchMock).toHaveBeenCalledTimes(1);

    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("https://ntfy.sh/v1/account/login");
    expect(options.method).toBe("POST");
    expect(options.headers.Authorization).toBe(`Basic ${btoa("phil@example.com:secret")}`);
  });

  it("throws when the server response has no token", async () => {
    fetchMock.mockResolvedValue(ok({}));
    await expect(accountApi.login({ username: "phil", password: "secret" })).rejects.toThrow("Cannot find token");
  });
});

describe("AccountApi.create", () => {
  it("POSTs username/password and defaults email to an empty string", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.create("phil", "pw");
    expect(fetchMock).toHaveBeenCalledWith(
      "https://ntfy.sh/v1/account",
      expect.objectContaining({ method: "POST", body: JSON.stringify({ username: "phil", password: "pw", email: "" }) }),
    );
  });
});

describe("AccountApi.get", () => {
  it("returns the parsed account and notifies the listener", async () => {
    fetchMock.mockResolvedValue(ok({ username: "phil" }));
    const listener = vi.fn();
    accountApi.registerListener(listener);

    const account = await accountApi.get();

    expect(account).toEqual({ username: "phil" });
    expect(listener).toHaveBeenCalledWith({ username: "phil" });
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("https://ntfy.sh/v1/account");
    expect(options.headers.Authorization).toBe("Bearer test-token");
  });
});

describe("AccountApi.changePassword", () => {
  it("POSTs the current and new passwords", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.changePassword("old", "new");
    const [, options] = fetchMock.mock.calls[0];
    expect(options.body).toBe(JSON.stringify({ password: "old", new_password: "new" }));
  });
});

describe("AccountApi.createToken", () => {
  it("converts a positive expiry into an absolute unix timestamp", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z")); // 1767225600 seconds
    fetchMock.mockResolvedValue(ok());

    await accountApi.createToken("my label", 3600);

    const [, options] = fetchMock.mock.calls[0];
    expect(JSON.parse(options.body)).toEqual({ label: "my label", expires: 1767225600 + 3600 });
  });

  it("sends expires=0 for a non-expiring token", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.createToken("forever", 0);
    const [, options] = fetchMock.mock.calls[0];
    expect(JSON.parse(options.body).expires).toBe(0);
  });
});

describe("AccountApi.deleteToken", () => {
  it("passes the target token in the X-Token header alongside the bearer token", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.deleteToken("tk_target");
    const [, options] = fetchMock.mock.calls[0];
    expect(options.method).toBe("DELETE");
    expect(options.headers["X-Token"]).toBe("tk_target");
    expect(options.headers.Authorization).toBe("Bearer test-token");
  });
});

describe("AccountApi subscriptions", () => {
  it("addSubscription POSTs base_url/topic and returns the parsed subscription", async () => {
    fetchMock.mockResolvedValue(ok({ id: "sub_1" }));
    const subscription = await accountApi.addSubscription("https://ntfy.sh", "mytopic");

    expect(subscription).toEqual({ id: "sub_1" });
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("https://ntfy.sh/v1/account/subscription");
    expect(options.method).toBe("POST");
    expect(JSON.parse(options.body)).toEqual({ base_url: "https://ntfy.sh", topic: "mytopic" });
  });

  it("deleteSubscription passes baseUrl/topic via headers", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.deleteSubscription("https://ntfy.sh", "mytopic");
    const [, options] = fetchMock.mock.calls[0];
    expect(options.method).toBe("DELETE");
    expect(options.headers["X-BaseURL"]).toBe("https://ntfy.sh");
    expect(options.headers["X-Topic"]).toBe("mytopic");
  });
});

describe("AccountApi.billingTiers", () => {
  it("caches the tiers and only fetches once", async () => {
    fetchMock.mockResolvedValue(ok([{ code: "pro" }]));
    const first = await accountApi.billingTiers();
    const second = await accountApi.billingTiers();

    expect(first).toEqual([{ code: "pro" }]);
    expect(second).toBe(first);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe("AccountApi.requestPasswordReset", () => {
  it("POSTs the identifier without any auth header", async () => {
    fetchMock.mockResolvedValue(ok());
    await accountApi.requestPasswordReset("phil");
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("https://ntfy.sh/v1/account/password/reset/request");
    expect(options.body).toBe(JSON.stringify({ identifier: "phil" }));
    expect(options.headers).toBeUndefined();
  });
});

describe("AccountApi.sync", () => {
  it("returns null and makes no request when there is no token", async () => {
    session.token.mockReturnValue(null);
    expect(await accountApi.sync()).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("resets the session and redirects to login on an unauthorized response", async () => {
    fetchMock.mockResolvedValue({ status: 401, json: async () => ({}) });
    const result = await accountApi.sync();

    expect(result).toBeUndefined();
    expect(session.resetAndRedirect).toHaveBeenCalledWith("/login");
  });

  it("applies language, notification prefs and subscriptions from the fetched account", async () => {
    fetchMock.mockResolvedValue(
      ok({
        language: "de",
        date_format: "iso8601",
        time_format: "24h",
        notification: { sound: "ding", delete_after: 3600, min_priority: 3 },
        subscriptions: [{ topic: "t" }],
        reservations: [{ topic: "t" }],
      }),
    );

    await accountApi.sync();

    expect(i18n.changeLanguage).toHaveBeenCalledWith("de");
    expect(prefs.setDateFormat).toHaveBeenCalledWith("iso8601");
    expect(prefs.setTimeFormat).toHaveBeenCalledWith("24h");
    expect(prefs.setSound).toHaveBeenCalledWith("ding");
    expect(prefs.setDeleteAfter).toHaveBeenCalledWith(3600);
    expect(prefs.setMinPriority).toHaveBeenCalledWith(3);
    expect(subscriptionManager.syncFromRemote).toHaveBeenCalledWith([{ topic: "t" }], [{ topic: "t" }]);
  });
});
