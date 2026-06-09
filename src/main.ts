import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  appendLogEntry,
  buildUsageSnippet,
  effectiveStatus,
  formatResetIn,
  formatTimestamp,
  formatUptime,
  modelMapFromLines,
  modelMapToLines,
  parsePositiveInt,
  RateLimitInfo,
  RequestLogEntry,
  serverUrl,
  ServerMetrics,
  ServerStatus,
  usageSummary,
} from "./view-model";

type Config = {
  bind_address: string;
  port: number;
  claude_binary_path: string;
  default_model: string;
  model_map: Record<string, string>;
  max_concurrency: number;
  request_timeout_secs: number;
  working_dir: string;
  require_auth: boolean;
};

type ClaudeAuthStatus = {
  logged_in: boolean;
  subscription_type?: string | null;
  account?: string | null;
  raw: unknown;
};

type KeyInfo = { id: string; label: string; prefix: string; created_at: number };
type CreatedKey = KeyInfo & { raw_key: string };

const AUTH_POLL_MS = 2000;
const AUTH_POLL_TIMEOUT_MS = 5 * 60 * 1000;

let config: Config | null = null;
let serverStatus: ServerStatus | null = null;
let metrics: ServerMetrics | null = null;
let limits: RateLimitInfo | null = null;
let authStatus: ClaudeAuthStatus | null = null;
let keys: KeyInfo[] = [];
let logs: RequestLogEntry[] = [];
let authPoll: number | null = null;
let authPolling = false;
let uptimeTick: number | null = null;

const $ = <T extends HTMLElement>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};

window.addEventListener("DOMContentLoaded", () => {
  void init();
});

async function init() {
  wireEvents();

  // Register live listeners BEFORE the initial loads so a single failing load
  // can never leave the server-status pill or log tail unwired.
  await listen<ServerStatus>("server_status", (event) => {
    serverStatus = event.payload;
    renderServer();
    renderConfigControls();
    void loadMetrics();
  });
  await listen<RequestLogEntry>("request_log", (event) => {
    logs = appendLogEntry(logs, event.payload);
    renderLogs();
  });
  await listen<ServerMetrics>("server_metrics", (event) => {
    metrics = event.payload;
    renderMetrics();
  });
  await listen<RateLimitInfo>("subscription_limits", (event) => {
    limits = event.payload;
    renderLimits();
  });

  // Load every panel independently; one failure surfaces in its own panel
  // instead of aborting the rest of init.
  await Promise.allSettled([
    loadConfig(),
    loadServerStatus(),
    loadAuthStatus(),
    loadKeys(),
    loadLogs(),
    loadMetrics(),
    loadLimits(),
  ]);
}

function wireEvents() {
  $("#start-server").addEventListener("click", () => void startServer());
  $("#stop-server").addEventListener("click", () => void stopServer());
  $("#recheck-auth").addEventListener("click", () => void loadAuthStatus());
  $("#open-login").addEventListener("click", () => void startLogin());
  $("#create-key").addEventListener("click", openKeyDialog);
  $("#key-cancel").addEventListener("click", () => $<HTMLDialogElement>("#key-dialog").close());
  $("#key-create-confirm").addEventListener("click", () => void confirmCreateKey());
  $("#key-label").addEventListener("keydown", (event) => {
    if ((event as KeyboardEvent).key === "Enter") void confirmCreateKey();
  });
  $("#check-limits").addEventListener("click", () => void refreshLimits());
  $("#save-config").addEventListener("click", () => void saveConfig());
  $("#callout-copy").addEventListener("click", () => void copyUsageSnippet());
  $("#raw-key-close").addEventListener("click", () => $<HTMLDialogElement>("#raw-key-dialog").close());
  $("#copy-raw-key").addEventListener("click", async () => {
    const raw = $("#raw-key").textContent ?? "";
    const ok = await copyToClipboard(raw);
    setText("#raw-key-copy-status", ok ? "Copied to clipboard." : "Copy failed — select the key above and copy it manually.");
  });
}

async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

async function loadConfig() {
  try {
    config = await invoke<Config>("get_config");
    renderConfigInputs();
    renderConfigControls();
    setText("#config-error", "");
  } catch (error) {
    setText("#config-error", `Failed to load config: ${error}`);
  }
}

async function loadServerStatus() {
  try {
    serverStatus = await invoke<ServerStatus>("get_server_status");
    renderServer();
    renderConfigControls();
  } catch (error) {
    setText("#server-error", `Failed to read server status: ${error}`);
  }
}

async function loadAuthStatus() {
  try {
    authStatus = await invoke<ClaudeAuthStatus>("get_claude_auth_status");
    renderAuth();
  } catch (error) {
    setText("#auth-error", `Failed to read Claude auth status: ${error}`);
  }
}

async function loadKeys() {
  try {
    keys = await invoke<KeyInfo[]>("list_api_keys");
    renderKeys();
  } catch (error) {
    setText("#keys-error", `Failed to load keys: ${error}`);
  }
}

async function loadLogs() {
  try {
    logs = await invoke<RequestLogEntry[]>("get_logs");
    renderLogs();
  } catch (error) {
    setText("#logs-empty", `Failed to load logs: ${error}`);
  }
}

async function loadMetrics() {
  try {
    metrics = await invoke<ServerMetrics>("get_server_metrics");
    renderMetrics();
  } catch (error) {
    setText("#metrics-error", `Failed to read server metrics: ${error}`);
  }
}

async function loadLimits() {
  try {
    limits = await invoke<RateLimitInfo | null>("get_subscription_limits");
    renderLimits();
  } catch (error) {
    setText("#limits-note", `Failed to read subscription limits: ${error}`);
  }
}

async function startServer() {
  setText("#server-error", "");
  try {
    serverStatus = await invoke<ServerStatus>("start_server");
    renderServer();
    renderConfigControls();
    await loadMetrics();
  } catch (error) {
    setText("#server-error", String(error));
  }
}

async function stopServer() {
  setText("#server-error", "");
  try {
    await invoke("stop_server");
    await loadServerStatus();
    await loadMetrics();
  } catch (error) {
    setText("#server-error", String(error));
  }
}

async function startLogin() {
  setText("#auth-error", "");
  try {
    await invoke("start_claude_login");
    startAuthPoll();
  } catch (error) {
    setText("#auth-error", String(error));
  }
}

function startAuthPoll() {
  if (authPoll !== null) window.clearInterval(authPoll);
  const startedAt = Date.now();
  authPoll = window.setInterval(async () => {
    if (authPolling) return; // skip overlapping ticks
    authPolling = true;
    try {
      await loadAuthStatus();
    } finally {
      authPolling = false;
    }
    if (authStatus?.logged_in || Date.now() - startedAt > AUTH_POLL_TIMEOUT_MS) {
      stopAuthPoll();
    }
  }, AUTH_POLL_MS);
}

function stopAuthPoll() {
  if (authPoll !== null) {
    window.clearInterval(authPoll);
    authPoll = null;
  }
}

function openKeyDialog() {
  setText("#keys-error", "");
  setText("#key-dialog-error", "");
  const input = $<HTMLInputElement>("#key-label");
  input.value = "";
  $<HTMLDialogElement>("#key-dialog").showModal();
  input.focus();
}

async function confirmCreateKey() {
  const input = $<HTMLInputElement>("#key-label");
  const label = input.value.trim();
  if (!label) {
    setText("#key-dialog-error", "Enter a label.");
    input.focus();
    return;
  }
  try {
    const created = await invoke<CreatedKey>("create_api_key", { label });
    $<HTMLDialogElement>("#key-dialog").close();
    await loadKeys();
    showRawKey(created.raw_key);
  } catch (error) {
    setText("#key-dialog-error", `Failed to create key: ${error}`);
  }
}

async function refreshLimits() {
  const button = $<HTMLButtonElement>("#check-limits");
  button.disabled = true;
  button.textContent = "Checking…";
  setText("#limits-note", "Running a one-off Claude call to read the current limits…");
  try {
    limits = await invoke<RateLimitInfo | null>("refresh_subscription_limits");
    renderLimits();
  } catch (error) {
    setText("#limits-note", `Could not read limits: ${error}`);
  } finally {
    button.disabled = false;
    button.textContent = "Check now";
  }
}

async function revokeKey(id: string) {
  setText("#keys-error", "");
  try {
    await invoke("revoke_api_key", { id });
    await loadKeys();
  } catch (error) {
    setText("#keys-error", `Failed to revoke key: ${error}`);
  }
}

async function saveConfig() {
  if (!config) return;
  setText("#config-error", "");
  setText("#config-success", "");

  let next: Config;
  try {
    next = {
      ...config,
      bind_address: $<HTMLInputElement>("#config-bind").value.trim(),
      claude_binary_path: $<HTMLInputElement>("#config-claude-path").value.trim(),
      default_model: $<HTMLInputElement>("#config-default-model").value.trim(),
      working_dir: $<HTMLInputElement>("#config-working-dir").value.trim(),
      require_auth: $<HTMLInputElement>("#config-require-auth").checked,
      port: parsePositiveInt($<HTMLInputElement>("#config-port").value, "Port", 65535),
      max_concurrency: parsePositiveInt($<HTMLInputElement>("#config-concurrency").value, "Max concurrency"),
      request_timeout_secs: parsePositiveInt($<HTMLInputElement>("#config-timeout").value, "Timeout seconds"),
      model_map: modelMapFromLines($<HTMLTextAreaElement>("#config-model-map").value),
    };
    if (!next.bind_address) throw new Error("Bind address is required.");
    if (!next.claude_binary_path) throw new Error("Claude binary path is required.");
    if (!next.default_model) throw new Error("Default model is required.");
  } catch (error) {
    setText("#config-error", error instanceof Error ? error.message : String(error));
    return;
  }

  try {
    await invoke("set_config", { config: next });
    config = next;
    renderConfigInputs();
    setText("#config-success", "Saved.");
  } catch (error) {
    setText("#config-error", String(error));
  }
}

async function copyUsageSnippet() {
  const url = serverUrl(effectiveStatus(serverStatus, config));
  const ok = await copyToClipboard(buildUsageSnippet(url));
  const button = $<HTMLButtonElement>("#callout-copy");
  button.textContent = ok ? "Copied" : "Copy failed";
  window.setTimeout(() => {
    button.textContent = "Copy usage snippet";
  }, 1500);
}

function renderServer() {
  if (!serverStatus) return;
  const pill = $("#server-pill");
  pill.textContent = serverStatus.running ? "Running" : "Stopped";
  pill.className = `pill ${serverStatus.running ? "ok" : "muted"}`;
  setText("#server-url", serverUrl(serverStatus));
  $<HTMLButtonElement>("#start-server").disabled = serverStatus.running;
  $<HTMLButtonElement>("#stop-server").disabled = !serverStatus.running;
}

function renderMetrics() {
  if (!metrics) return;
  const running = metrics.running;
  setText("#metric-total", String(metrics.total_requests));
  setText("#metric-active", `${metrics.active_requests} / ${metrics.max_concurrency}`);
  renderUptime();
  if (running && metrics.started_at) {
    startUptimeTick();
  } else {
    stopUptimeTick();
    setText("#metric-uptime", "—");
  }
}

function renderUptime() {
  if (!metrics?.running || !metrics.started_at) {
    setText("#metric-uptime", "—");
    return;
  }
  const secs = Math.max(0, Math.floor((Date.now() - metrics.started_at) / 1000));
  setText("#metric-uptime", formatUptime(secs));
}

function startUptimeTick() {
  if (uptimeTick !== null) return;
  uptimeTick = window.setInterval(renderUptime, 1000);
}

function stopUptimeTick() {
  if (uptimeTick !== null) {
    window.clearInterval(uptimeTick);
    uptimeTick = null;
  }
}

function renderLimits() {
  const card = $("#limits-card");
  if (!limits) {
    card.classList.add("empty");
    setText("#limits-note", "No data yet — limits populate after the first proxied request.");
    return;
  }
  card.classList.remove("empty");
  const usingOverage = limits.is_using_overage === true;
  const pill = $("#limits-pill");
  pill.textContent = usingOverage ? "Using overage" : limits.status ?? "unknown";
  pill.className = `pill ${usingOverage ? "bad" : limits.status === "allowed" ? "ok" : "muted"}`;

  setText("#limit-window", limits.rate_limit_type ? limits.rate_limit_type.replace(/_/g, " ") : "—");
  const nowSecs = Math.floor(Date.now() / 1000);
  setText("#limit-resets", limits.resets_at ? formatResetIn(limits.resets_at, nowSecs) : "—");
  setText("#limit-overage", limits.overage_status ?? "—");
  setText(
    "#limits-note",
    `As of ${formatTimestamp(limits.captured_at)} · from the latest proxied request.`,
  );
}

function renderAuth() {
  if (!authStatus) return;
  const pill = $("#auth-pill");
  pill.textContent = authStatus.logged_in ? "Logged in" : "Not logged in";
  pill.className = `pill ${authStatus.logged_in ? "ok" : "bad"}`;
  setText("#auth-account", authStatus.account ?? "Unknown account");
  setText("#auth-plan", authStatus.subscription_type ?? "Unknown plan");
}

function renderKeys() {
  const body = $("#keys-body");
  body.replaceChildren(
    ...keys.map((key) => {
      const row = document.createElement("tr");
      row.innerHTML = `<td>${escapeHtml(key.label)}</td><td><code>${escapeHtml(key.prefix)}…</code></td><td>${formatTimestamp(key.created_at)}</td><td class="row-action"></td>`;
      const button = document.createElement("button");
      button.textContent = "Revoke";
      button.className = "danger small";
      button.addEventListener("click", () => void revokeKey(key.id));
      row.querySelector(".row-action")?.append(button);
      return row;
    }),
  );
  setText("#keys-empty", keys.length === 0 ? "No keys yet. Create one to let clients authenticate." : "");
}

/** Repopulate every input from the saved config. Only call on load/save so a
 * live server_status event never clobbers the operator's unsaved edits. */
function renderConfigInputs() {
  if (!config) return;
  $<HTMLInputElement>("#config-bind").value = config.bind_address;
  $<HTMLInputElement>("#config-port").value = String(config.port);
  $<HTMLInputElement>("#config-claude-path").value = config.claude_binary_path;
  $<HTMLInputElement>("#config-default-model").value = config.default_model;
  $<HTMLInputElement>("#config-concurrency").value = String(config.max_concurrency);
  $<HTMLInputElement>("#config-timeout").value = String(config.request_timeout_secs);
  $<HTMLInputElement>("#config-working-dir").value = config.working_dir;
  $<HTMLInputElement>("#config-require-auth").checked = config.require_auth;
  $<HTMLTextAreaElement>("#config-model-map").value = modelMapToLines(config.model_map).join("\n");
}

/** Enable/disable Save based purely on server state — no input rewrites. */
function renderConfigControls() {
  const running = serverStatus?.running ?? false;
  $<HTMLButtonElement>("#save-config").disabled = running;
  setText("#config-hint", running ? "Stop the server before changing config." : "");
}

function renderLogs() {
  const body = $("#logs-body");
  body.replaceChildren(
    ...logs
      .slice()
      .reverse()
      .map((entry) => {
        const row = document.createElement("tr");
        const model = entry.mapped_model
          ? `${escapeHtml(entry.client_model ?? "")} → ${escapeHtml(entry.mapped_model)}`
          : escapeHtml(entry.client_model ?? "");
        row.innerHTML = [
          `<td>${formatTimestamp(entry.ts)}</td>`,
          `<td>${escapeHtml(entry.method)}</td>`,
          `<td><code>${escapeHtml(entry.path)}</code></td>`,
          `<td>${model}</td>`,
          `<td><span class="status s${Math.floor(entry.status / 100)}">${entry.status}</span></td>`,
          `<td>${entry.duration_ms}</td>`,
          `<td>${escapeHtml(usageSummary(entry.usage))}</td>`,
        ].join("");
        return row;
      }),
  );
  setText("#logs-empty", logs.length === 0 ? "No requests yet." : "");
}

function showRawKey(rawKey: string) {
  setText("#raw-key", rawKey);
  setText("#raw-key-copy-status", "");
  $<HTMLDialogElement>("#raw-key-dialog").showModal();
}

function setText(selector: string, value: string) {
  $(selector).textContent = value;
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => {
    const replacements: Record<string, string> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      "\"": "&quot;",
      "'": "&#039;",
    };
    return replacements[char];
  });
}
