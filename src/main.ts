import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  appendLogEntry,
  formatTimestamp,
  modelMapFromLines,
  modelMapToLines,
  RequestLogEntry,
  serverUrl,
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

type KeyInfo = {
  id: string;
  label: string;
  prefix: string;
  created_at: number;
};

type CreatedKey = KeyInfo & {
  raw_key: string;
};

let config: Config | null = null;
let serverStatus: ServerStatus | null = null;
let authStatus: ClaudeAuthStatus | null = null;
let keys: KeyInfo[] = [];
let logs: RequestLogEntry[] = [];
let authPoll: number | null = null;

const $ = <T extends HTMLElement>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};

window.addEventListener("DOMContentLoaded", async () => {
  wireEvents();
  await Promise.all([loadConfig(), loadServerStatus(), loadAuthStatus(), loadKeys(), loadLogs()]);
  await listen<ServerStatus>("server_status", (event) => {
    serverStatus = event.payload;
    renderServer();
    renderConfig();
  });
  await listen<RequestLogEntry>("request_log", (event) => {
    logs = appendLogEntry(logs, event.payload);
    renderLogs();
  });
});

function wireEvents() {
  $("#start-server").addEventListener("click", startServer);
  $("#stop-server").addEventListener("click", stopServer);
  $("#recheck-auth").addEventListener("click", loadAuthStatus);
  $("#open-login").addEventListener("click", startLogin);
  $("#create-key").addEventListener("click", createKey);
  $("#save-config").addEventListener("click", saveConfig);
  $("#callout-copy").addEventListener("click", copyUsageSnippet);
  $("#raw-key-close").addEventListener("click", () => {
    ($<HTMLDialogElement>("#raw-key-dialog")).close();
  });
  $("#copy-raw-key").addEventListener("click", async () => {
    const rawKey = $("#raw-key").textContent ?? "";
    await navigator.clipboard.writeText(rawKey);
    $("#raw-key-copy-status").textContent = "Copied";
  });
}

async function loadConfig() {
  config = await invoke<Config>("get_config");
  renderConfig();
}

async function loadServerStatus() {
  serverStatus = await invoke<ServerStatus>("get_server_status");
  renderServer();
  renderConfig();
}

async function loadAuthStatus() {
  authStatus = await invoke<ClaudeAuthStatus>("get_claude_auth_status");
  renderAuth();
}

async function loadKeys() {
  keys = await invoke<KeyInfo[]>("list_api_keys");
  renderKeys();
}

async function loadLogs() {
  logs = await invoke<RequestLogEntry[]>("get_logs");
  renderLogs();
}

async function startServer() {
  setText("#server-error", "");
  try {
    serverStatus = await invoke<ServerStatus>("start_server");
    renderServer();
    renderConfig();
  } catch (error) {
    setText("#server-error", String(error));
  }
}

async function stopServer() {
  setText("#server-error", "");
  try {
    await invoke("stop_server");
    await loadServerStatus();
  } catch (error) {
    setText("#server-error", String(error));
  }
}

async function startLogin() {
  setText("#auth-error", "");
  try {
    await invoke("start_claude_login");
    if (authPoll !== null) window.clearInterval(authPoll);
    authPoll = window.setInterval(async () => {
      await loadAuthStatus();
      if (authStatus?.logged_in && authPoll !== null) {
        window.clearInterval(authPoll);
        authPoll = null;
      }
    }, 2000);
  } catch (error) {
    setText("#auth-error", String(error));
  }
}

async function createKey() {
  const label = window.prompt("Label for this API key");
  if (!label?.trim()) return;

  const created = await invoke<CreatedKey>("create_api_key", { label: label.trim() });
  await loadKeys();
  showRawKey(created.raw_key);
}

async function revokeKey(id: string) {
  await invoke("revoke_api_key", { id });
  await loadKeys();
}

async function saveConfig() {
  if (!config) return;
  setText("#config-error", "");
  try {
    const next: Config = {
      ...config,
      bind_address: ($<HTMLInputElement>("#config-bind")).value.trim(),
      port: Number(($<HTMLInputElement>("#config-port")).value),
      claude_binary_path: ($<HTMLInputElement>("#config-claude-path")).value.trim(),
      default_model: ($<HTMLInputElement>("#config-default-model")).value.trim(),
      max_concurrency: Number(($<HTMLInputElement>("#config-concurrency")).value),
      request_timeout_secs: Number(($<HTMLInputElement>("#config-timeout")).value),
      working_dir: ($<HTMLInputElement>("#config-working-dir")).value.trim(),
      require_auth: ($<HTMLInputElement>("#config-require-auth")).checked,
      model_map: modelMapFromLines(($<HTMLTextAreaElement>("#config-model-map")).value),
    };
    await invoke("set_config", { config: next });
    config = next;
    renderConfig();
    setText("#config-success", "Saved");
  } catch (error) {
    setText("#config-success", "");
    setText("#config-error", String(error));
  }
}

function renderServer() {
  if (!serverStatus) return;
  $("#server-pill").textContent = serverStatus.running ? "Running" : "Stopped";
  $("#server-pill").className = `pill ${serverStatus.running ? "ok" : "muted"}`;
  setText("#server-url", serverUrl(serverStatus));
  ($<HTMLButtonElement>("#start-server")).disabled = serverStatus.running;
  ($<HTMLButtonElement>("#stop-server")).disabled = !serverStatus.running;
}

function renderAuth() {
  if (!authStatus) return;
  $("#auth-pill").textContent = authStatus.logged_in ? "Logged in" : "Not logged in";
  $("#auth-pill").className = `pill ${authStatus.logged_in ? "ok" : "bad"}`;
  setText("#auth-account", authStatus.account ?? "Unknown account");
  setText("#auth-plan", authStatus.subscription_type ?? "Unknown plan");
}

function renderKeys() {
  const body = $("#keys-body");
  body.replaceChildren(
    ...keys.map((key) => {
      const row = document.createElement("tr");
      row.innerHTML = `<td>${escapeHtml(key.label)}</td><td><code>${escapeHtml(key.prefix)}...</code></td><td>${formatTimestamp(key.created_at)}</td><td></td>`;
      const button = document.createElement("button");
      button.textContent = "Revoke";
      button.className = "danger";
      button.addEventListener("click", () => revokeKey(key.id));
      row.lastElementChild?.append(button);
      return row;
    }),
  );
}

function renderConfig() {
  if (!config) return;
  ($<HTMLInputElement>("#config-bind")).value = config.bind_address;
  ($<HTMLInputElement>("#config-port")).value = String(config.port);
  ($<HTMLInputElement>("#config-claude-path")).value = config.claude_binary_path;
  ($<HTMLInputElement>("#config-default-model")).value = config.default_model;
  ($<HTMLInputElement>("#config-concurrency")).value = String(config.max_concurrency);
  ($<HTMLInputElement>("#config-timeout")).value = String(config.request_timeout_secs);
  ($<HTMLInputElement>("#config-working-dir")).value = config.working_dir;
  ($<HTMLInputElement>("#config-require-auth")).checked = config.require_auth;
  ($<HTMLTextAreaElement>("#config-model-map")).value = modelMapToLines(config.model_map).join("\n");

  const running = serverStatus?.running ?? false;
  ($<HTMLButtonElement>("#save-config")).disabled = running;
  setText("#config-hint", running ? "Stop the server before changing config." : "");
}

function renderLogs() {
  const body = $("#logs-body");
  body.replaceChildren(
    ...logs.slice().reverse().map((entry) => {
      const row = document.createElement("tr");
      row.innerHTML = [
        `<td>${formatTimestamp(entry.ts)}</td>`,
        `<td>${escapeHtml(entry.method)}</td>`,
        `<td>${escapeHtml(entry.path)}</td>`,
        `<td>${escapeHtml(entry.client_model ?? "")} ${entry.mapped_model ? `→ ${escapeHtml(entry.mapped_model)}` : ""}</td>`,
        `<td>${entry.status}</td>`,
        `<td>${entry.duration_ms}</td>`,
        `<td>${escapeHtml(usageSummary(entry.usage))}</td>`,
      ].join("");
      return row;
    }),
  );
}

function showRawKey(rawKey: string) {
  setText("#raw-key", rawKey);
  setText("#raw-key-copy-status", "");
  ($<HTMLDialogElement>("#raw-key-dialog")).showModal();
}

async function copyUsageSnippet() {
  const status = serverStatus ?? config
    ? { running: false, bind: config?.bind_address ?? "0.0.0.0", port: config?.port ?? 8787 }
    : { running: false, bind: "0.0.0.0", port: 8787 };
  const snippet = buildUsageSnippet(serverUrl(status));
  await navigator.clipboard.writeText(snippet);
  setText("#callout-copy", "Copied");
  window.setTimeout(() => {
    const button = $<HTMLButtonElement>("#callout-copy");
    if (button) button.textContent = "Copy usage snippet";
  }, 1500);
}

function buildUsageSnippet(url: string): string {
  return [
    "OpenAI-compatible:",
    `  curl ${url}/chat/completions \\`,
    "    -H 'Authorization: Bearer csp-<your-key>' \\",
    "    -H 'Content-Type: application/json' \\",
    "    -d '{\"model\":\"gpt-4o\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}'",
    "",
    "Anthropic-compatible:",
    `  curl ${url}/messages \\`,
    "    -H 'x-api-key: csp-<your-key>' \\",
    "    -H 'anthropic-version: 2023-06-01' \\",
    "    -H 'Content-Type: application/json' \\",
    "    -d '{\"model\":\"claude-sonnet-4-5\",\"max_tokens\":50,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}'",
  ].join("\n");
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
