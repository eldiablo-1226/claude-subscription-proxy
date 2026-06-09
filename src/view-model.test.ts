import { describe, expect, it } from "vitest";
import {
  appendLogEntry,
  buildUsageSnippet,
  effectiveStatus,
  formatResetIn,
  formatUptime,
  modelMapFromLines,
  modelMapToLines,
  parsePositiveInt,
  serverUrl,
  usageSummary,
} from "./view-model";

describe("dashboard view model helpers", () => {
  it("round trips editable model map lines", () => {
    const lines = modelMapToLines({ "gpt-4o": "opus", "gpt-4o-mini": "haiku" });

    expect(lines).toEqual(["gpt-4o=opus", "gpt-4o-mini=haiku"]);
    expect(modelMapFromLines(lines.join("\n"))).toEqual({
      "gpt-4o": "opus",
      "gpt-4o-mini": "haiku",
    });
  });

  it("ignores blank model map rows and trims whitespace", () => {
    expect(modelMapFromLines(" gpt-4 = opus \n\n gpt-3.5-turbo=haiku ")).toEqual({
      "gpt-4": "opus",
      "gpt-3.5-turbo": "haiku",
    });
  });

  it("formats server URL from status", () => {
    expect(serverUrl({ running: true, bind: "0.0.0.0", port: 8787 })).toBe("http://0.0.0.0:8787");
  });

  it("keeps log tail capped at 500 rows", () => {
    const log = Array.from({ length: 500 }, (_, ts) => ({ ts }));
    const next = appendLogEntry(log, { ts: 501 });

    expect(next).toHaveLength(500);
    expect(next[0]).toEqual({ ts: 1 });
    expect(next[499]).toEqual({ ts: 501 });
  });
});

describe("effectiveStatus", () => {
  it("prefers the live server status when present", () => {
    const status = effectiveStatus(
      { running: true, bind: "0.0.0.0", port: 9999 },
      { bind_address: "127.0.0.1", port: 8787 },
    );
    expect(status).toEqual({ running: true, bind: "0.0.0.0", port: 9999 });
  });

  it("falls back to config when there is no live status", () => {
    const status = effectiveStatus(null, { bind_address: "127.0.0.1", port: 8787 });
    expect(status).toEqual({ running: false, bind: "127.0.0.1", port: 8787 });
  });

  it("falls back to defaults when nothing is loaded", () => {
    expect(effectiveStatus(null, null)).toEqual({ running: false, bind: "0.0.0.0", port: 8787 });
  });
});

describe("buildUsageSnippet", () => {
  it("uses the given base URL for both API shapes", () => {
    const snippet = buildUsageSnippet("http://192.168.1.10:9999");
    expect(snippet).toContain("http://192.168.1.10:9999/v1/chat/completions");
    expect(snippet).toContain("http://192.168.1.10:9999/v1/messages");
    expect(snippet).toContain("x-api-key");
  });
});

describe("parsePositiveInt", () => {
  it("accepts in-range integers", () => {
    expect(parsePositiveInt("8787", "Port", 65535)).toBe(8787);
    expect(parsePositiveInt(" 4 ", "Concurrency")).toBe(4);
  });

  it("rejects empty, zero, non-numeric, and out-of-range values", () => {
    expect(() => parsePositiveInt("", "Port", 65535)).toThrow();
    expect(() => parsePositiveInt("0", "Concurrency")).toThrow();
    expect(() => parsePositiveInt("abc", "Port", 65535)).toThrow();
    expect(() => parsePositiveInt("70000", "Port", 65535)).toThrow();
    expect(() => parsePositiveInt("1.5", "Concurrency")).toThrow();
  });
});

describe("formatUptime", () => {
  it("formats durations across units", () => {
    expect(formatUptime(0)).toBe("0s");
    expect(formatUptime(45)).toBe("45s");
    expect(formatUptime(90)).toBe("1m 30s");
    expect(formatUptime(3600)).toBe("1h 0m");
    expect(formatUptime(3661)).toBe("1h 1m");
    expect(formatUptime(90061)).toBe("1d 1h");
  });
});

describe("formatResetIn", () => {
  it("returns 'now' when the reset moment has passed", () => {
    expect(formatResetIn(100, 200)).toBe("now");
    expect(formatResetIn(200, 200)).toBe("now");
  });

  it("formats a future reset relative to now", () => {
    expect(formatResetIn(200 + 3661, 200)).toBe("in 1h 1m");
    expect(formatResetIn(200 + 90, 200)).toBe("in 1m 30s");
  });
});

describe("modelMapFromLines edge cases", () => {
  it("returns an empty map for blank input", () => {
    expect(modelMapFromLines("")).toEqual({});
    expect(modelMapFromLines("\n  \n")).toEqual({});
  });

  it("throws on rows without a key=value separator or with empty sides", () => {
    expect(() => modelMapFromLines("gpt-4o")).toThrow();
    expect(() => modelMapFromLines("=opus")).toThrow();
    expect(() => modelMapFromLines("gpt-4o=")).toThrow();
  });
});

describe("usageSummary", () => {
  it("reads OpenAI and Anthropic token field names", () => {
    expect(usageSummary({ prompt_tokens: 7, completion_tokens: 2 })).toBe("7 / 2");
    expect(usageSummary({ input_tokens: 5, output_tokens: 9 })).toBe("5 / 9");
    expect(usageSummary({ input_tokens: 5 })).toBe("5 / 0");
  });

  it("returns empty string for missing or non-object usage", () => {
    expect(usageSummary(null)).toBe("");
    expect(usageSummary("nope")).toBe("");
    expect(usageSummary({ other: 1 })).toBe("");
  });
});

describe("parsePositiveInt messages", () => {
  it("describes an upper bound when one is given", () => {
    expect(() => parsePositiveInt("0", "Port", 65535)).toThrow(/between 1 and 65535/);
  });

  it("describes an open range when no max is given", () => {
    expect(() => parsePositiveInt("0", "Concurrency")).toThrow(/1 or greater/);
  });
});
