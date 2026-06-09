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

export function serverUrl(status: ServerStatus): string {
  return `http://${status.bind}:${status.port}`;
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
      throw new Error(`Invalid model map row: ${line}`);
    }
    const key = line.slice(0, separator).trim();
    const value = line.slice(separator + 1).trim();
    if (!key || !value) {
      throw new Error(`Invalid model map row: ${line}`);
    }
    modelMap[key] = value;
  }
  return modelMap;
}

export function appendLogEntry<T>(entries: T[], entry: T): T[] {
  const next = [...entries, entry];
  return next.length > 500 ? next.slice(next.length - 500) : next;
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
    return `${prompt ?? 0}/${completion ?? 0}`;
  }
  return "";
}
