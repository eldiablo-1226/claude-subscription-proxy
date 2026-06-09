import { describe, expect, it } from "vitest";
import { appendLogEntry, modelMapFromLines, modelMapToLines, serverUrl } from "./view-model";

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
