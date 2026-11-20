const elements = Object.fromEntries(
  [...document.querySelectorAll("[id]")].map((element) => [element.id, element]),
);

const providerNames = { google_drive: "Google Drive", one_drive: "OneDrive" };
const statusLabels = {
  preparing: "Preparing connection…",
  waiting_for_authorization: "Waiting for authorization. Complete sign-in in your browser.",
  question: "",
  verifying_access: "Verifying drive access…",
  cancelling: "Cancelling…",
  cancelled: "Connection cancelled.",
  connected: "",
  failed: "Connection failed.",
  reconnect_required: "Reconnect required.",
};
const inProgress = new Set([
  "preparing", "waiting_for_authorization", "question", "verifying_access", "cancelling",
]);

let connection;
let webdav;
let renderedConnection = false;
let renderedSessionId;
let questionId;
let timer;
let revision = 0;
let submitting = false;
let syncRemoteId;
let syncBusy = false;
let syncBaseline;
let advancedSettings;
let advancedRemoteId;
let advancedLoading = false;
let advancedSaving = false;

async function request(path, options) {
  const response = await fetch(path, options);
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    const error = new Error(data.message || "Could not reach the Bridge dashboard.");
    error.status = response.status;
    throw error;
  }
  return response.json();
}

function render() {
  const active = connection?.active;
  const session = connection?.session;
  const phase = session?.phase;
  const waiting = inProgress.has(phase);
  const provider = elements.provider;

  if ((session || active) && (!renderedConnection || renderedSessionId !== session?.id)) {
    provider.value = (session || active).provider;
  }
  renderedConnection = Boolean(connection);
  renderedSessionId = session?.id;
  provider.disabled = !connection || !webdav || waiting || submitting || webdavActive() || syncBusy || advancedSaving;
  elements.connect.disabled = provider.disabled;
  elements.active.dataset.connected = Boolean(active);
  const activeLabel = active
    ? "Connected · " + providerNames[active.provider]
    : connection ? "Disconnected" : "Checking connection…";
  if (elements.active.textContent !== activeLabel) elements.active.textContent = activeLabel;
  renderWebDav(active, waiting);
  renderAdvanced(active, waiting);
  renderSyncControls(active, waiting);
  elements.progress.hidden = !phase || phase === "connected";
  const status = [statusLabels[phase], session?.message].filter(Boolean).join(" ");
  if (elements.phase.textContent !== status) elements.phase.textContent = status;
  elements.phase.hidden = !status;
  elements.phase.dataset.failed = phase === "failed" || phase === "reconnect_required";
  elements.cancel.hidden = !waiting;
  elements.cancel.disabled = submitting || phase === "cancelling";
  elements.continue.disabled = submitting;

  const question = session?.question;
  elements["question-form"].hidden = !question;
  if (questionId !== question?.id) {
    questionId = question?.id;
    elements.prompt.textContent = question?.prompt || "";
    const choices = (question?.choices || []).map((choice) => {
      const label = document.createElement("label");
      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "choice";
      radio.value = choice.id;
      radio.required = true;
      label.append(radio, document.createTextNode(choice.label));
      return label;
    });
    elements.choices.replaceChildren(...choices);
  }
}

const webdavErrors = {
  settings_read: "Could not read the saved WebDAV settings.",
  settings_save: "Could not save the WebDAV settings.",
  password_generation: "Could not generate a WebDAV password.",
  folder_create: "Could not create the Zotero folder on the connected drive.",
  folder_access: "Could not access the Zotero folder on the connected drive.",
  folder_timeout: "Timed out while preparing the Zotero folder.",
  launch: "Could not start rclone.",
  port_in_use: "rclone could not bind a WebDAV port.",
  port_unavailable: "Bridge could not use the selected WebDAV port.",
  readiness_timeout: "Timed out waiting for WebDAV to respond.",
  credentials_rejected: "The WebDAV server rejected the saved credentials.",
  readiness_probe: "The WebDAV server did not pass the PROPFIND check.",
  monitor: "Could not check the rclone process.",
};

function webdavActive() {
  return ["starting", "applying", "running"].includes(webdav?.status);
}

function renderWebDav(active, connecting) {
  const endpoint = webdav?.endpoint;
  const busy = ["starting", "applying"].includes(webdav?.status);
  elements.webdav.disabled = !webdav || !active || connecting || busy || submitting || syncBusy || advancedSaving;
  elements.webdav.setAttribute("aria-checked", String(webdavActive()));
  const status = {
    stopped: active ? "Stopped." : "Connect a cloud provider to enable WebDAV.",
    starting: "Starting WebDAV…",
    applying: "Applying WebDAV settings…",
    running: "Running. Turn off WebDAV before changing the cloud provider.",
    failed: webdav?.error?.code === "exited"
      ? "rclone exited (" + webdav.error.detail + ")."
      : webdavErrors[webdav?.error?.code] || "WebDAV failed.",
  }[webdav?.status] || "Checking WebDAV status…";
  const message = elements["webdav-message"];
  if (message.textContent !== status) message.textContent = status;
  message.dataset.failed = webdav?.status === "failed";
  elements["webdav-details"].hidden = !endpoint;
  elements["webdav-port-changed"].hidden = !endpoint?.port_changed;
  for (const [id, value] of [
    ["webdav-url", endpoint?.url],
    ["webdav-user", endpoint?.username],
    ["webdav-password", endpoint?.password],
  ]) {
    const field = elements[id];
    if (field.value !== (value || "")) field.value = value || "";
  }
  if (!endpoint) {
    elements["webdav-show-password"].checked = false;
    elements["webdav-password"].type = "password";
  }
}

function showMessage(id, message) {
  const field = elements[id];
  if (field.textContent !== message) field.textContent = message;
  field.hidden = !message;
}

function renderAdvanced(active, connecting) {
  if (advancedRemoteId !== active?.id) {
    elements.advanced.open = false;
    advancedRemoteId = active?.id;
    advancedSettings = undefined;
    elements["advanced-password"].value = "";
    showMessage("advanced-error", "");
  }
  if (!active && elements.advanced.open) {
    showMessage("advanced-status", "Connect a cloud provider to edit WebDAV settings.");
  }
  elements["advanced-form"].hidden = !advancedSettings;
  elements["advanced-fields"].disabled = !advancedSettings || advancedLoading || advancedSaving
    || connecting || submitting || syncBusy || ["starting", "applying"].includes(webdav?.status);
}

function renderPreferredPort() {
  const preferred = elements["advanced-mode"].value === "preferred";
  elements["advanced-port-label"].hidden = !preferred;
  elements["advanced-port"].hidden = !preferred;
  elements["advanced-port"].disabled = !preferred;
}

function populateAdvanced(settings) {
  advancedSettings = settings;
  elements["advanced-folder"].value = settings.cloud_folder;
  elements["advanced-mode"].value = settings.port.mode;
  elements["advanced-port"].value = settings.port.number ?? settings.last_port ?? "";
  elements["advanced-username"].value = settings.username;
  elements["advanced-password"].value = settings.password ?? "";
  renderPreferredPort();
}

async function loadAdvanced() {
  if (!elements.advanced.open || !advancedRemoteId || advancedLoading || advancedSaving) return;
  const remoteId = advancedRemoteId;
  advancedSettings = undefined;
  advancedLoading = true;
  showMessage("advanced-status", "Loading settings…");
  showMessage("advanced-error", "");
  render();
  try {
    const settings = await request("/api/settings");
    if (settings.remote_id === remoteId && advancedRemoteId === remoteId) {
      populateAdvanced(settings);
      showMessage("advanced-status", "");
    }
  } catch (error) {
    showMessage("advanced-status", "");
    showMessage("advanced-error", error.message);
  } finally {
    advancedLoading = false;
    render();
  }
}

async function applyAdvanced(event) {
  event.preventDefault();
  if (elements["advanced-fields"].disabled) return;
  const previous = advancedSettings;
  const mode = elements["advanced-mode"].value;
  const password = elements["advanced-password"].value;
  const update = {
    remote_id: previous.remote_id,
    cloud_folder: elements["advanced-folder"].value,
    port: mode === "preferred"
      ? { mode, number: Number(elements["advanced-port"].value) }
      : { mode },
    username: elements["advanced-username"].value,
    password: password && password !== previous.password ? password : null,
  };
  advancedSaving = true;
  showMessage("advanced-status", "Applying settings…");
  showMessage("advanced-error", "");
  render();
  try {
    const result = await request("/api/settings", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(update),
    });
    populateAdvanced(result.settings);
    if (previous.cloud_folder !== result.settings.cloud_folder) clearSyncResults();
    webdav = result.webdav;
    const saved = result.webdav.status === "running"
      ? "Settings applied. WebDAV is running."
      : "Settings saved for the next WebDAV startup.";
    const reverify = result.update_zotero
      ? " Update the URL or credentials that changed in Zotero, then choose Verify Server."
      : "";
    showMessage("advanced-status", saved + reverify);
  } catch (error) {
    showMessage("advanced-status", "");
    showMessage("advanced-error", error.message);
  } finally {
    advancedSaving = false;
    render();
  }
}

function clearSyncResults() {
  syncBaseline = undefined;
  elements["sync-results"].hidden = true;
  elements["sync-objects"].replaceChildren();
  elements["sync-error"].hidden = true;
}

function renderSyncControls(active, connecting) {
  if (syncRemoteId !== active?.id) {
    syncRemoteId = active?.id;
    clearSyncResults();
  }
  const disabled = !active || connecting || submitting || syncBusy || advancedSaving || webdav?.status === "applying";
  elements["sync-baseline"].disabled = disabled;
  elements["sync-check"].disabled = disabled;
}

async function checkCloudFolder(captureBaseline) {
  const remoteId = connection?.active?.id;
  if (!remoteId || syncBusy) return;

  syncBusy = true;
  const progress = elements["sync-progress"];
  const alert = elements["sync-error"];
  progress.textContent = "Listing cloud objects…";
  progress.hidden = false;
  alert.hidden = true;
  render();
  try {
    const listing = await request("/api/sync-objects", { method: "POST" });
    if (listing.remote_id !== remoteId || syncRemoteId !== remoteId) return;
    if (syncBaseline?.folder !== listing.folder) syncBaseline = undefined;
    if (captureBaseline) {
      syncBaseline = {
        folder: listing.folder,
        paths: new Set(listing.objects.map((object) => object.path)),
      };
    }
    renderCloudListing(listing, captureBaseline);
  } catch (error) {
    if (syncRemoteId === remoteId) {
      alert.textContent = error.message;
      alert.hidden = false;
    }
  } finally {
    syncBusy = false;
    progress.hidden = true;
    render();
  }
}

function renderCloudListing(listing, captureBaseline) {
  const objects = listing.objects;
  const newPaths = new Set();
  if (syncBaseline && !captureBaseline) {
    for (const { path } of objects) {
      if (!syncBaseline.paths.has(path)) newPaths.add(path);
    }
  }

  const count = objects.length + (objects.length === 1 ? " object" : " objects");
  const summary = [listing.folder + " · " + count + "."];
  if (captureBaseline) {
    summary.push("Baseline captured. Import and sync the test PDF, then check this folder again.");
  } else if (syncBaseline && newPaths.size > 0) {
    summary.push("New paths since baseline: " + newPaths.size + ". These are candidates for the test upload.");
  } else if (syncBaseline) {
    summary.push("No new paths since baseline. Check again after Zotero finishes syncing.");
  }
  elements["sync-summary"].textContent = summary.join(" ");

  const rows = document.createDocumentFragment();
  const formatter = new Intl.NumberFormat();
  const sorted = [
    ...objects.filter(({ path }) => newPaths.has(path)),
    ...objects.filter(({ path }) => !newPaths.has(path)),
  ];
  for (const object of sorted) {
    const row = document.createElement("tr");
    const path = row.insertCell();
    path.textContent = object.path;
    if (newPaths.has(object.path)) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = "New";
      path.prepend(badge);
    }
    row.insertCell().textContent = object.size_bytes === null ? "Unknown" : formatter.format(object.size_bytes);
    rows.append(row);
  }
  elements["sync-objects"].replaceChildren(rows);
  elements["sync-table"].hidden = objects.length === 0;
  elements["sync-results"].hidden = false;
}

function showError(message) {
  elements["request-error"].textContent = message;
  elements["request-error"].hidden = !message;
}

function pollLater() {
  clearTimeout(timer);
  timer = setTimeout(poll, 800);
}

async function poll() {
  const started = revision;
  const session = connection?.session;
  const path = session && inProgress.has(session.phase)
    ? "/api/connections/" + session.id
    : "/api/connection";
  try {
    const [connectionData, webdavData] = await Promise.all([
      request(path).catch((error) => {
        if (error.status === 404) return request("/api/connection");
        throw error;
      }),
      request("/api/webdav"),
    ]);
    if (started === revision) {
      connection = connectionData;
      webdav = webdavData;
      render();
    }
  } catch (error) {
    if (started === revision) showError(error.message);
  } finally {
    if (started === revision) pollLater();
  }
}

async function post(path, body, update = (data) => { connection = data; }, reportError = showError) {
  if (submitting) return;
  clearTimeout(timer);
  revision++;
  submitting = true;
  reportError("");
  render();
  try {
    const data = await request(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    update(data);
  } catch (error) {
    reportError(error.message);
  } finally {
    submitting = false;
    render();
    pollLater();
  }
}

elements["connect-form"].addEventListener("submit", (event) => {
  event.preventDefault();
  post("/api/connections", { provider: elements.provider.value });
});

elements["question-form"].addEventListener("submit", (event) => {
  event.preventDefault();
  const data = new FormData(event.currentTarget);
  post("/api/connections/" + connection.session.id + "/answer", {
    question_id: questionId,
    value: Number(data.get("choice")),
  });
});

elements.cancel.addEventListener("click", () => {
  post("/api/connections/" + connection.session.id + "/cancel", {});
});

elements.webdav.addEventListener("click", () => {
  const path = webdav?.status === "running" ? "/api/webdav/stop" : "/api/webdav";
  post(path, {}, (data) => { webdav = data; }, (message) => showMessage("webdav-error", message));
});

elements["webdav-show-password"].addEventListener("change", (event) => {
  elements["webdav-password"].type = event.currentTarget.checked ? "text" : "password";
});

elements["sync-baseline"].addEventListener("click", () => checkCloudFolder(true));
elements["sync-check"].addEventListener("click", () => checkCloudFolder(false));
elements.advanced.addEventListener("toggle", loadAdvanced);
elements["advanced-mode"].addEventListener("change", renderPreferredPort);
elements["advanced-form"].addEventListener("submit", applyAdvanced);

poll();
