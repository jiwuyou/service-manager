(() => {
  "use strict";

  const API_PREFIX = "/api/v1";
  const LS_TOKEN = "service_manager_token";

  const $ = (id) => {
    const el = document.getElementById(id);
    if (!el) throw new Error(`missing element: ${id}`);
    return el;
  };

  const els = {
    serverOrigin: $("serverOrigin"),
    connChip: $("connChip"),
    globalError: $("globalError"),
    globalInfo: $("globalInfo"),

    tokenInput: $("tokenInput"),
    btnTokenToggle: $("btnTokenToggle"),
    btnTokenSave: $("btnTokenSave"),
    btnTokenClear: $("btnTokenClear"),

    svcSearch: $("svcSearch"),
    btnCreateService: $("btnCreateService"),
    btnRefreshAll: $("btnRefreshAll"),
    servicesList: $("servicesList"),
    servicesMeta: $("servicesMeta"),

    tabService: $("tabService"),
    tabProviders: $("tabProviders"),
    viewService: $("viewService"),
    viewProviders: $("viewProviders"),

    serviceEmpty: $("serviceEmpty"),
    servicePanel: $("servicePanel"),
    svcTitle: $("svcTitle"),
    svcSubtitle: $("svcSubtitle"),
    svcStateChip: $("svcStateChip"),
    svcStatusKv: $("svcStatusKv"),
    svcLogs: $("svcLogs"),
    logsLimit: $("logsLimit"),
    svcSpecJson: $("svcSpecJson"),

    btnSvcEdit: $("btnSvcEdit"),
    btnSvcRegister: $("btnSvcRegister"),
    btnSvcUnregister: $("btnSvcUnregister"),
    btnSvcStart: $("btnSvcStart"),
    btnSvcStop: $("btnSvcStop"),
    btnSvcRestart: $("btnSvcRestart"),
    btnSvcDelete: $("btnSvcDelete"),
    btnRefreshStatus: $("btnRefreshStatus"),
    btnFetchLogs: $("btnFetchLogs"),

    providersMeta: $("providersMeta"),
    providersTable: $("providersTable"),

    svcModal: $("svcModal"),
    svcForm: $("svcForm"),
    svcModalTitle: $("svcModalTitle"),
    btnModalClose: $("btnModalClose"),
    btnFormCancel: $("btnFormCancel"),
    btnFormSave: $("btnFormSave"),
    formError: $("formError"),

    fName: $("fName"),
    fProvider: $("fProvider"),
    fDescription: $("fDescription"),
    fCommand: $("fCommand"),
    fWorkingDir: $("fWorkingDir"),
    fEnv: $("fEnv"),
    fRuntime: $("fRuntime"),
    fRestartMode: $("fRestartMode"),
    fMaxRetries: $("fMaxRetries"),
    fEnabled: $("fEnabled"),
    fTags: $("fTags"),
    btnAddHealth: $("btnAddHealth"),
    healthList: $("healthList"),
  };

  const state = {
    token: "",
    online: false,
    providers: [],
    services: [],
    selectedServiceId: null,
    selectedService: null,
    selectedStatus: null,
    selectedLogs: [],
    busy: new Set(),
    editMode: null, // "create" | "edit"
    editId: null,
  };

  class ApiError extends Error {
    constructor(status, code, message) {
      super(message);
      this.name = "ApiError";
      this.status = status;
      this.code = code || "unknown_error";
    }
  }

  const setBusy = (key, v) => {
    if (v) state.busy.add(key);
    else state.busy.delete(key);
  };
  const isBusy = (key) => state.busy.has(key);

  const setNotice = (type, msg) => {
    const box = type === "error" ? els.globalError : els.globalInfo;
    const other = type === "error" ? els.globalInfo : els.globalError;
    other.classList.add("hidden");
    other.textContent = "";

    if (!msg) {
      box.classList.add("hidden");
      box.textContent = "";
      return;
    }
    box.textContent = msg;
    box.classList.remove("hidden");
  };

  const clearNotices = () => {
    setNotice("error", "");
    setNotice("info", "");
  };

  const fmtTime = (s) => {
    if (!s) return "";
    const d = new Date(s);
    if (Number.isNaN(d.getTime())) return String(s);
    return d.toLocaleString();
  };

  const esc = (s) => String(s == null ? "" : s);

  const tokenLoad = () => {
    const t = localStorage.getItem(LS_TOKEN);
    return t ? t.trim() : "";
  };

  const tokenSave = (tok) => {
    const t = (tok || "").trim();
    if (!t) localStorage.removeItem(LS_TOKEN);
    else localStorage.setItem(LS_TOKEN, t);
  };

  const apiFetch = async (path, { method = "GET", query = null, json = null } = {}) => {
    const url = new URL(window.location.origin + path);
    if (query) {
      for (const [k, v] of Object.entries(query)) {
        if (v == null) continue;
        const sv = String(v).trim();
        if (!sv) continue;
        url.searchParams.set(k, sv);
      }
    }

    const headers = { Accept: "application/json" };
    if (state.token) headers.Authorization = `Bearer ${state.token}`;
    if (json != null) headers["Content-Type"] = "application/json";

    let res;
    try {
      res = await fetch(url.toString(), {
        method,
        headers,
        body: json == null ? null : JSON.stringify(json),
      });
    } catch (e) {
      throw new ApiError(0, "network_error", e && e.message ? e.message : "network error");
    }

    if (res.status === 204) return null;

    const ctype = (res.headers.get("content-type") || "").toLowerCase();
    const isJson = ctype.includes("application/json");
    const body = isJson ? await res.json().catch(() => null) : await res.text().catch(() => "");

    if (!res.ok) {
      if (isJson && body && body.error && body.error.message) {
        throw new ApiError(res.status, body.error.code, body.error.message);
      }
      throw new ApiError(res.status, "http_error", typeof body === "string" ? body : "http error");
    }

    return body;
  };

  const healthPing = async () => {
    try {
      await apiFetch(`${API_PREFIX}/health`, { method: "GET" });
      state.online = true;
    } catch {
      state.online = false;
    }
    renderConnChip();
  };

  const renderConnChip = () => {
    const chip = els.connChip;
    chip.textContent = state.online ? "Online" : "Offline";
    chip.classList.remove("chip--ok", "chip--bad", "chip--neutral");
    chip.classList.add(state.online ? "chip--ok" : "chip--bad");
  };

  const showApiError = (e) => {
    if (e instanceof ApiError) {
      if (e.status === 401) {
        setNotice("error", "Unauthorized: missing/invalid bearer token.");
        return;
      }
      if (e.status === 0) {
        setNotice("error", `Network error: ${e.message}`);
        return;
      }
      setNotice("error", `${e.code}: ${e.message}`);
      return;
    }
    setNotice("error", e && e.message ? e.message : "unexpected error");
  };

  const withBusyButton = async (btn, key, fn) => {
    if (!btn) return fn();
    if (isBusy(key)) return;
    setBusy(key, true);
    btn.disabled = true;
    clearNotices();
    try {
      return await fn();
    } finally {
      setBusy(key, false);
      btn.disabled = false;
    }
  };

  const loadProviders = async () => {
    setBusy("providers", true);
    try {
      const out = await apiFetch(`${API_PREFIX}/providers`);
      state.providers = Array.isArray(out) ? out : [];
    } finally {
      setBusy("providers", false);
    }
  };

  const loadServices = async () => {
    setBusy("services", true);
    try {
      const out = await apiFetch(`${API_PREFIX}/services`);
      state.services = Array.isArray(out) ? out : [];
    } finally {
      setBusy("services", false);
    }
  };

  const getProviderInfo = (providerId) => {
    const pid = typeof providerId === "string" ? providerId : providerId && providerId.id;
    return state.providers.find((p) => (p.id && p.id === pid) || p.id === providerId) || null;
  };

  const providerCapabilities = (providerId) => {
    const p = getProviderInfo(providerId);
    const caps = p && Array.isArray(p.capabilities) ? p.capabilities : [];
    return new Set(caps.map((c) => String(c)));
  };

  const renderProviders = () => {
    const tbody = els.providersTable.querySelector("tbody");
    tbody.textContent = "";

    els.providersMeta.textContent = `${state.providers.length}`;

    for (const p of state.providers) {
      const tr = document.createElement("tr");

      const tdId = document.createElement("td");
      tdId.textContent = p.id || "";
      tr.appendChild(tdId);

      const tdName = document.createElement("td");
      tdName.textContent = p.display_name || "";
      tr.appendChild(tdName);

      const tdDetected = document.createElement("td");
      const chip = document.createElement("span");
      chip.className = "chip " + (p.detected ? "chip--ok" : "chip--bad");
      chip.textContent = p.detected ? "yes" : "no";
      tdDetected.appendChild(chip);
      tr.appendChild(tdDetected);

      const tdCaps = document.createElement("td");
      const caps = Array.isArray(p.capabilities) ? p.capabilities : [];
      tdCaps.textContent = caps.join(", ");
      tr.appendChild(tdCaps);

      const tdNotes = document.createElement("td");
      const parts = [];
      if (p.detect_error) parts.push(`error: ${p.detect_error}`);
      if (p.detect_details) parts.push(String(p.detect_details));
      tdNotes.textContent = parts.join(" | ");
      tr.appendChild(tdNotes);

      tbody.appendChild(tr);
    }
  };

  const renderServicesList = () => {
    const q = els.svcSearch.value.trim().toLowerCase();
    const services = [...state.services];
    services.sort((a, b) => {
      const an = (a && a.spec && a.spec.name ? a.spec.name : "").toLowerCase();
      const bn = (b && b.spec && b.spec.name ? b.spec.name : "").toLowerCase();
      return an.localeCompare(bn);
    });

    const filtered = q
      ? services.filter((s) => {
          const name = (s && s.spec && s.spec.name ? s.spec.name : "").toLowerCase();
          const id = (s && s.id ? s.id : "").toLowerCase();
          const provider = (s && s.spec && s.spec.provider ? s.spec.provider : "").toLowerCase();
          return name.includes(q) || id.includes(q) || provider.includes(q);
        })
      : services;

    els.servicesList.textContent = "";
    els.servicesMeta.textContent = `${filtered.length} of ${services.length}`;

    for (const svc of filtered) {
      const id = svc && svc.id ? String(svc.id) : "";
      const name = svc && svc.spec && svc.spec.name ? String(svc.spec.name) : id;
      const provider = svc && svc.spec && svc.spec.provider ? String(svc.spec.provider) : "";

      const item = document.createElement("div");
      item.className = "listItem" + (state.selectedServiceId === id ? " is-active" : "");
      item.dataset.serviceId = id;

      const left = document.createElement("div");
      const title = document.createElement("div");
      title.className = "listItem__title";
      title.textContent = name;
      const sub = document.createElement("div");
      sub.className = "listItem__sub";
      sub.textContent = `${id} • ${provider}`;
      left.appendChild(title);
      left.appendChild(sub);
      item.appendChild(left);

      const right = document.createElement("div");
      const enabled = !!(svc && svc.spec && svc.spec.enabled);
      const chip = document.createElement("span");
      chip.className = "chip " + (enabled ? "chip--ok" : "chip--warn");
      chip.textContent = enabled ? "enabled" : "disabled";
      right.appendChild(chip);
      item.appendChild(right);

      item.addEventListener("click", () => selectService(id));
      els.servicesList.appendChild(item);
    }
  };

  const renderSelectedService = () => {
    const svc = state.selectedService;
    if (!svc) {
      els.serviceEmpty.classList.remove("hidden");
      els.servicePanel.classList.add("hidden");
      return;
    }

    els.serviceEmpty.classList.add("hidden");
    els.servicePanel.classList.remove("hidden");

    const name = svc.spec && svc.spec.name ? svc.spec.name : svc.id;
    els.svcTitle.textContent = name;

    const provider = svc.spec && svc.spec.provider ? svc.spec.provider : "";
    const updatedAt = svc.updated_at ? fmtTime(svc.updated_at) : "";
    const createdAt = svc.created_at ? fmtTime(svc.created_at) : "";
    els.svcSubtitle.textContent = `${svc.id} • provider=${provider} • created=${createdAt} • updated=${updatedAt}`;

    const st = state.selectedStatus;
    const stateName = st && st.state ? String(st.state) : "unknown";
    els.svcStateChip.textContent = stateName;
    els.svcStateChip.classList.remove("chip--neutral", "chip--ok", "chip--warn", "chip--bad");
    let chipClass = "chip--neutral";
    if (stateName === "running") chipClass = "chip--ok";
    else if (stateName === "failed") chipClass = "chip--bad";
    else if (stateName === "starting" || stateName === "stopping") chipClass = "chip--warn";
    els.svcStateChip.classList.add(chipClass);

    renderStatusKv();
    renderLogs();
    els.svcSpecJson.textContent = JSON.stringify(svc.spec || {}, null, 2);

    // Enable/disable actions based on provider capabilities.
    const caps = providerCapabilities(provider);
    const disableAll = !state.token || !state.online;
    const setBtn = (btn, capOrNull) => {
      const allowed = capOrNull ? caps.has(capOrNull) : true;
      btn.disabled = disableAll || !allowed;
      btn.title = allowed ? "" : "Provider does not support this capability";
    };

    setBtn(els.btnSvcEdit, null);
    setBtn(els.btnSvcDelete, null);
    setBtn(els.btnSvcRegister, "register");
    setBtn(els.btnSvcUnregister, "unregister");
    setBtn(els.btnSvcStart, "start");
    setBtn(els.btnSvcStop, "stop");
    setBtn(els.btnSvcRestart, "restart");
    els.btnRefreshStatus.disabled = disableAll || !caps.has("status");
    els.btnFetchLogs.disabled = disableAll || !caps.has("logs");
  };

  const renderStatusKv = () => {
    const kv = els.svcStatusKv;
    kv.textContent = "";
    const st = state.selectedStatus;
    if (!st) {
      const v = document.createElement("div");
      v.className = "muted";
      v.textContent = "No status loaded.";
      kv.appendChild(v);
      return;
    }

    const rows = [
      ["state", st.state],
      ["message", st.message || ""],
      ["pid", st.pid == null ? "" : String(st.pid)],
      ["exit_code", st.exit_code == null ? "" : String(st.exit_code)],
      ["observed_at", fmtTime(st.observed_at)],
      ["started_at", fmtTime(st.started_at)],
      ["provider", st.provider || ""],
    ];
    for (const [k, v] of rows) {
      const dk = document.createElement("div");
      dk.className = "kv__k";
      dk.textContent = k;
      const dv = document.createElement("div");
      dv.className = "kv__v";
      dv.textContent = esc(v);
      kv.appendChild(dk);
      kv.appendChild(dv);
    }
  };

  const renderLogs = () => {
    const pre = els.svcLogs;
    const logs = Array.isArray(state.selectedLogs) ? state.selectedLogs : [];
    if (!logs.length) {
      pre.textContent = "";
      return;
    }
    const lines = [];
    for (const e of logs) {
      const t = e && e.time ? e.time : "";
      const stream = e && e.stream ? e.stream : "unknown";
      const msg = e && e.message ? e.message : "";
      lines.push(`${t} [${stream}] ${msg}`);
    }
    pre.textContent = lines.join("\n");
    pre.scrollTop = pre.scrollHeight;
  };

  const selectService = async (id) => {
    state.selectedServiceId = id;
    renderServicesList();
    await refreshSelectedService();
  };

  const refreshSelectedService = async () => {
    const id = state.selectedServiceId;
    if (!id) {
      state.selectedService = null;
      state.selectedStatus = null;
      state.selectedLogs = [];
      renderSelectedService();
      return;
    }

    setBusy("service_get", true);
    try {
      const svc = await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}`);
      state.selectedService = svc;
      state.selectedStatus = null;
      state.selectedLogs = [];
      renderSelectedService();
      const provider = svc && svc.spec && svc.spec.provider ? String(svc.spec.provider) : "";
      if (providerCapabilities(provider).has("status")) {
        await refreshStatus();
      }
    } catch (e) {
      showApiError(e);
      state.selectedService = null;
      state.selectedStatus = null;
      state.selectedLogs = [];
      renderSelectedService();
    } finally {
      setBusy("service_get", false);
    }
  };

  const refreshStatus = async () => {
    const id = state.selectedServiceId;
    if (!id) return;
    try {
      const st = await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}/status`);
      state.selectedStatus = st;
      renderSelectedService();
    } catch (e) {
      showApiError(e);
    }
  };

  const fetchLogs = async () => {
    const id = state.selectedServiceId;
    if (!id) return;
    const limit = Number.parseInt(els.logsLimit.value, 10);
    const q = {};
    if (Number.isFinite(limit) && limit > 0) q.limit = String(limit);

    try {
      const logs = await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}/logs`, {
        query: q,
      });
      state.selectedLogs = Array.isArray(logs) ? logs : [];
      renderSelectedService();
    } catch (e) {
      showApiError(e);
    }
  };

  const svcAction = async (action, { confirmMsg = null, btn = null } = {}) => {
    const id = state.selectedServiceId;
    if (!id) return;
    if (confirmMsg && !window.confirm(confirmMsg)) return;

    const url = `${API_PREFIX}/services/${encodeURIComponent(id)}/${action}`;
    await withBusyButton(btn, `svc_${action}`, async () => {
      try {
        await apiFetch(url, { method: "POST" });
        setNotice("info", `${action}: ok`);
        await refreshStatus();
      } catch (e) {
        showApiError(e);
      }
    });
  };

  const svcDelete = async () => {
    const id = state.selectedServiceId;
    if (!id) return;
    if (!window.confirm("Delete this service? This cannot be undone.")) return;

    try {
      await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}`, { method: "DELETE" });
      setNotice("info", "deleted");
      state.selectedServiceId = null;
      state.selectedService = null;
      state.selectedStatus = null;
      state.selectedLogs = [];
      await loadServices();
      renderServicesList();
      renderSelectedService();
    } catch (e) {
      showApiError(e);
    }
  };

  const openModal = (mode, svcOrNull) => {
    state.editMode = mode;
    state.editId = svcOrNull && svcOrNull.id ? String(svcOrNull.id) : null;
    els.svcModalTitle.textContent = mode === "create" ? "Create service" : "Edit service";

    // Providers select.
    els.fProvider.textContent = "";
    for (const p of state.providers) {
      const opt = document.createElement("option");
      opt.value = p.id || "";
      opt.textContent = `${p.id || ""}${p.display_name ? ` (${p.display_name})` : ""}`;
      els.fProvider.appendChild(opt);
    }

    const spec = svcOrNull && svcOrNull.spec ? svcOrNull.spec : null;
    els.fName.value = spec && spec.name ? spec.name : "";
    els.fDescription.value = spec && spec.description ? spec.description : "";
    els.fProvider.value = spec && spec.provider ? spec.provider : (state.providers[0] && state.providers[0].id) || "";
    els.fCommand.value = spec && Array.isArray(spec.command) ? spec.command.join("\n") : "";
    els.fWorkingDir.value = spec && spec.working_dir ? spec.working_dir : "";

    els.fEnabled.checked = spec ? !!spec.enabled : true;

    const tags = spec && Array.isArray(spec.tags) ? spec.tags : [];
    els.fTags.value = tags.join(", ");

    const env = spec && spec.env ? spec.env : {};
    const envLines = [];
    for (const [k, v] of Object.entries(env)) envLines.push(`${k}=${v}`);
    els.fEnv.value = envLines.join("\n");

    const runtime = spec && spec.runtime ? spec.runtime : {};
    els.fRuntime.value = JSON.stringify(runtime || {}, null, 2);

    const restart = spec && spec.restart ? spec.restart : {};
    els.fRestartMode.value = restart && restart.mode ? restart.mode : "no";
    els.fMaxRetries.value =
      restart && typeof restart.max_retries === "number" ? String(restart.max_retries) : "0";

    renderHealthEditor(spec && Array.isArray(spec.health) ? spec.health : []);

    els.formError.classList.add("hidden");
    els.formError.textContent = "";

    els.svcModal.showModal();
  };

  const closeModal = () => {
    if (els.svcModal.open) els.svcModal.close();
  };

  const renderHealthEditor = (checks) => {
    const list = els.healthList;
    list.textContent = "";

    const normalized = Array.isArray(checks) ? checks : [];
    for (const hc of normalized) {
      addHealthRow(hc);
    }
  };

  const addHealthRow = (hc) => {
    const row = document.createElement("div");
    row.className = "healthRow";

    const ty = document.createElement("select");
    ty.className = "input";
    const optHttp = document.createElement("option");
    optHttp.value = "http";
    optHttp.textContent = "http";
    const optTcp = document.createElement("option");
    optTcp.value = "tcp";
    optTcp.textContent = "tcp";
    ty.appendChild(optHttp);
    ty.appendChild(optTcp);
    ty.value = hc && hc.type ? hc.type : (hc && hc.ty ? hc.ty : "http");

    const interval = document.createElement("input");
    interval.className = "input";
    interval.placeholder = "interval (e.g. 5s)";
    interval.value = hc && hc.interval ? String(hc.interval) : "";

    const timeout = document.createElement("input");
    timeout.className = "input";
    timeout.placeholder = "timeout (e.g. 2s)";
    timeout.value = hc && hc.timeout ? String(hc.timeout) : "";

    const target = document.createElement("input");
    target.className = "input";
    target.placeholder = ty.value === "tcp" ? "address (host:port)" : "url (http://...)";
    target.value = hc && (hc.url || hc.address) ? String(hc.url || hc.address) : "";

    const rm = document.createElement("button");
    rm.className = "btn btn--small healthRow__remove";
    rm.type = "button";
    rm.textContent = "Remove";
    rm.addEventListener("click", () => row.remove());

    ty.addEventListener("change", () => {
      target.placeholder = ty.value === "tcp" ? "address (host:port)" : "url (http://...)";
    });

    row.appendChild(ty);
    row.appendChild(interval);
    row.appendChild(timeout);
    row.appendChild(target);
    row.appendChild(rm);

    els.healthList.appendChild(row);
  };

  const parseEnvLines = (s) => {
    const out = {};
    const lines = String(s || "").split("\n");
    for (let i = 0; i < lines.length; i++) {
      const raw = lines[i].trim();
      if (!raw) continue;
      const eq = raw.indexOf("=");
      if (eq <= 0) throw new Error(`env line ${i + 1}: expected KEY=VALUE`);
      const k = raw.slice(0, eq).trim();
      const v = raw.slice(eq + 1);
      if (!k) throw new Error(`env line ${i + 1}: empty KEY`);
      out[k] = v;
    }
    return out;
  };

  const parseTags = (s) => {
    const raw = String(s || "")
      .split(",")
      .map((x) => x.trim())
      .filter((x) => x.length > 0);
    // Unique while preserving order.
    const seen = new Set();
    const out = [];
    for (const t of raw) {
      if (seen.has(t)) continue;
      seen.add(t);
      out.push(t);
    }
    return out;
  };

  const parseCommand = (s) => {
    const parts = String(s || "")
      .split("\n")
      .map((x) => x.trim())
      .filter((x) => x.length > 0);
    return parts;
  };

  const validateServiceName = (name) => {
    const re = /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/;
    return re.test(name);
  };

  const buildSpecFromForm = () => {
    const name = els.fName.value.trim();
    if (!name) throw new Error("name is required");
    if (!validateServiceName(name)) {
      throw new Error("name must match ^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$");
    }

    const provider = els.fProvider.value.trim();
    if (!provider) throw new Error("provider is required");

    const cmd = parseCommand(els.fCommand.value);

    const env = parseEnvLines(els.fEnv.value);

    let runtime = {};
    const rtRaw = els.fRuntime.value.trim();
    if (rtRaw) {
      try {
        runtime = JSON.parse(rtRaw);
      } catch (e) {
        throw new Error(`runtime JSON: ${e && e.message ? e.message : "invalid JSON"}`);
      }
      if (runtime == null || Array.isArray(runtime) || typeof runtime !== "object") {
        throw new Error("runtime options must be a JSON object");
      }
    }

    const mode = els.fRestartMode.value;
    const mr = Number.parseInt(els.fMaxRetries.value, 10);
    if (!Number.isFinite(mr) || mr < 0) throw new Error("max_retries must be >= 0");

    const health = [];
    for (const row of els.healthList.querySelectorAll(".healthRow")) {
      const inputs = row.querySelectorAll("select,input");
      if (inputs.length < 4) continue;
      const ty = inputs[0].value;
      const interval = inputs[1].value.trim();
      const timeout = inputs[2].value.trim();
      const target = inputs[3].value.trim();

      if (!target) continue; // treat empty row as ignored

      const hc = { type: ty };
      if (interval) hc.interval = interval;
      if (timeout) hc.timeout = timeout;
      if (ty === "tcp") hc.address = target;
      else hc.url = target;
      health.push(hc);
    }

    return {
      name,
      description: els.fDescription.value || "",
      provider,
      command: cmd,
      working_dir: els.fWorkingDir.value || "",
      env,
      runtime,
      restart: { mode, max_retries: mr },
      health,
      enabled: !!els.fEnabled.checked,
      tags: parseTags(els.fTags.value),
    };
  };

  const submitForm = async () => {
    els.formError.classList.add("hidden");
    els.formError.textContent = "";

    let spec;
    try {
      spec = buildSpecFromForm();
    } catch (e) {
      els.formError.textContent = e && e.message ? e.message : "invalid form";
      els.formError.classList.remove("hidden");
      return;
    }

    if (state.editMode === "create") {
      const svc = await apiFetch(`${API_PREFIX}/services`, { method: "POST", json: spec });
      closeModal();
      setNotice("info", "created");
      await loadServices();
      renderServicesList();
      if (svc && svc.id) await selectService(String(svc.id));
      return;
    }

    if (state.editMode === "edit" && state.editId) {
      await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(state.editId)}`, {
        method: "PUT",
        json: spec,
      });
      closeModal();
      setNotice("info", "updated");
      await loadServices();
      renderServicesList();
      await refreshSelectedService();
      return;
    }

    els.formError.textContent = "invalid edit mode";
    els.formError.classList.remove("hidden");
  };

  const switchTab = (tab) => {
    for (const btn of [els.tabService, els.tabProviders]) {
      btn.classList.toggle("is-active", btn.dataset.tab === tab);
    }
    for (const v of [els.viewService, els.viewProviders]) {
      v.classList.toggle("is-active", v.dataset.view === tab);
    }
  };

  const refreshAll = async () => {
    await withBusyButton(els.btnRefreshAll, "refresh_all", async () => {
      await healthPing();
      if (!state.token) {
        setNotice("error", "Set a token to access /api/v1 endpoints.");
        return;
      }
      try {
        await Promise.all([loadProviders(), loadServices()]);
        renderProviders();
        renderServicesList();
        renderSelectedService();
      } catch (e) {
        showApiError(e);
      }
    });
  };

  const init = async () => {
    els.serverOrigin.textContent = window.location.origin;
    els.logsLimit.value = "200";

    state.token = tokenLoad();
    els.tokenInput.value = state.token;
    renderConnChip();

    els.btnTokenToggle.addEventListener("click", () => {
      const inp = els.tokenInput;
      const isPw = inp.type === "password";
      inp.type = isPw ? "text" : "password";
      els.btnTokenToggle.textContent = isPw ? "Hide" : "Show";
    });

    els.btnTokenSave.addEventListener("click", async () => {
      state.token = els.tokenInput.value.trim();
      tokenSave(state.token);
      setNotice("info", state.token ? "token saved" : "token cleared");
      await refreshAll();
    });

    els.btnTokenClear.addEventListener("click", async () => {
      els.tokenInput.value = "";
      state.token = "";
      tokenSave("");
      setNotice("info", "token cleared");
      await refreshAll();
    });

    els.svcSearch.addEventListener("input", () => renderServicesList());
    els.btnRefreshAll.addEventListener("click", () => refreshAll());

    els.tabService.addEventListener("click", () => switchTab("service"));
    els.tabProviders.addEventListener("click", () => switchTab("providers"));

    els.btnCreateService.addEventListener("click", () => openModal("create", null));

    els.btnSvcEdit.addEventListener("click", () => {
      if (state.selectedService) openModal("edit", state.selectedService);
    });

    els.btnSvcRegister.addEventListener("click", () => svcAction("register", { btn: els.btnSvcRegister }));
    els.btnSvcUnregister.addEventListener("click", () =>
      svcAction("unregister", { btn: els.btnSvcUnregister }),
    );
    els.btnSvcStart.addEventListener("click", () => svcAction("start", { btn: els.btnSvcStart }));
    els.btnSvcStop.addEventListener("click", () => svcAction("stop", { btn: els.btnSvcStop }));
    els.btnSvcRestart.addEventListener("click", () => svcAction("restart", { btn: els.btnSvcRestart }));
    els.btnSvcDelete.addEventListener("click", () =>
      withBusyButton(els.btnSvcDelete, "svc_delete", () => svcDelete()),
    );
    els.btnRefreshStatus.addEventListener("click", () =>
      withBusyButton(els.btnRefreshStatus, "status", () => refreshStatus()),
    );
    els.btnFetchLogs.addEventListener("click", () =>
      withBusyButton(els.btnFetchLogs, "logs", () => fetchLogs()),
    );

    els.btnModalClose.addEventListener("click", () => closeModal());
    els.btnFormCancel.addEventListener("click", () => closeModal());
    els.btnAddHealth.addEventListener("click", () => addHealthRow({ type: "http" }));

    els.svcForm.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      await withBusyButton(els.btnFormSave, "form_save", async () => {
        try {
          await submitForm();
        } catch (e) {
          showApiError(e);
        }
      });
    });

    // Close modal on ESC without treating it as form submission.
    els.svcModal.addEventListener("cancel", (ev) => {
      ev.preventDefault();
      closeModal();
    });

    await refreshAll();
    setInterval(() => void healthPing(), 15000);
  };

  window.addEventListener("load", () => void init());
})();
