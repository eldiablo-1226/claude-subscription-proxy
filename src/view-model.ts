export type ServerStatus = {
  running: boolean;
  bind: string;
  port: number;
};

export type RequestLogEntry = {
  ts: number;
  method: string;
  path: string;
  client_model?: string | null;
  mapped_model?: string | null;
  status: number;
  duration_ms: number;
  usage?: unknown;
};

export const DEFAULT_BIND = "0.0.0.0";
export const DEFAULT_PORT = 8787;
export const MAX_LOG_ROWS = 500;

export function serverUrl(status: ServerStatus): string {
  return `http://${status.bind}:${status.port}`;
}

/**
 * The status to display / build the usage snippet from: prefer the live server
 * status, fall back to the saved config, then to defaults. (Replaces a broken
 * `serverStatus ?? config ? a : b` precedence that ignored serverStatus.)
 */
export function effectiveStatus(
  status: ServerStatus | null,
  config: { bind_address: string; port: number } | null,
): ServerStatus {
  if (status) return status;
  return {
    running: false,
    bind: config?.bind_address ?? DEFAULT_BIND,
    port: config?.port ?? DEFAULT_PORT,
  };
}

export function modelMapToLines(modelMap: Record<string, string>): string[] {
  return Object.entries(modelMap)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([client, mapped]) => `${client}=${mapped}`);
}

export function modelMapFromLines(lines: string): Record<string, string> {
  const modelMap: Record<string, string> = {};
  for (const rawLine of lines.split("\n")) {
    const line = rawLine.trim();
    if (!line) continue;
    const separator = line.indexOf("=");
    if (separator < 1) {
      throw new Error(`Invalid model map row (expected client=model): ${line}`);
    }
    const key = line.slice(0, separator).trim();
    const value = line.slice(separator + 1).trim();
    if (!key || !value) {
      throw new Error(`Invalid model map row (expected client=model): ${line}`);
    }
    modelMap[key] = value;
  }
  return modelMap;
}

/** Parse a required positive integer form field, throwing a user-facing message. */
export function parsePositiveInt(raw: string, label: string, max?: number): number {
  const value = Number(raw.trim());
  const upperOk = max === undefined || value <= max;
  if (!Number.isInteger(value) || value < 1 || !upperOk) {
    const ceiling = max === undefined ? "" : ` and ${max}`;
    throw new Error(`${label} must be a whole number between 1${ceiling}.`);
  }
  return value;
}

export function appendLogEntry<T>(entries: T[], entry: T): T[] {
  const next = [...entries, entry];
  return next.length > MAX_LOG_ROWS ? next.slice(next.length - MAX_LOG_ROWS) : next;
}

export function formatTimestamp(epochMillis: number): string {
  return new Date(epochMillis).toLocaleTimeString();
}

export function usageSummary(usage: unknown): string {
  if (!usage || typeof usage !== "object") return "";
  const record = usage as Record<string, unknown>;
  const prompt = record.prompt_tokens ?? record.input_tokens;
  const completion = record.completion_tokens ?? record.output_tokens;
  if (typeof prompt === "number" || typeof completion === "number") {
    return `${prompt ?? 0} / ${completion ?? 0}`;
  }
  return "";
}

/** Copy-paste client setup for both API shapes, pointed at the live base URL. */
export function buildUsageSnippet(url: string): string {
  return [
    "# OpenAI-compatible (base_url = " + url + "/v1)",
    `curl ${url}/v1/chat/completions \\`,
    "  -H 'Authorization: Bearer csp-<your-key>' \\",
    "  -H 'Content-Type: application/json' \\",
    `  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'`,
    "",
    "# Anthropic-compatible (base_url = " + url + ")",
    `curl ${url}/v1/messages \\`,
    "  -H 'x-api-key: csp-<your-key>' \\",
    "  -H 'anthropic-version: 2023-06-01' \\",
    "  -H 'Content-Type: application/json' \\",
    `  -d '{"model":"claude-sonnet-4-5","max_tokens":64,"messages":[{"role":"user","content":"hi"}]}'`,
  ].join("\n");
}

export type ServerMetrics = {
  running: boolean;
  bind: string;
  port: number;
  started_at: number | null;
  uptime_secs: number;
  total_requests: number;
  active_requests: number;
  max_concurrency: number;
};

export type RateLimitInfo = {
  status?: string | null;
  rate_limit_type?: string | null;
  resets_at?: number | null;
  overage_status?: string | null;
  overage_resets_at?: number | null;
  is_using_overage?: boolean | null;
  captured_at: number;
};

/** Human-readable duration: "45s", "1m 30s", "1h 1m", "1d 1h". */
export function formatDuration(totalSecs: number): string {
  const s = Math.max(0, Math.floor(totalSecs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

export const formatUptime = formatDuration;

/** "in 1h 1m" until the reset epoch (seconds), or "now" once it has passed. */
export function formatResetIn(resetsAtSecs: number, nowSecs: number): string {
  const diff = resetsAtSecs - nowSecs;
  return diff <= 0 ? "now" : `in ${formatDuration(diff)}`;
}
