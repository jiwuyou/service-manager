(() => {
  "use strict";

  const API_PREFIX = "/api/v1";
  const LS_TOKEN = "service_manager_token";
  const GROUP_TAG_PREFIX = "group:";

  const STATE_LABELS = {
    unknown: "未知",
    stopped: "已停止",
    starting: "启动中",
    running: "运行中",
    stopping: "停止中",
    failed: "失败",
  };

  const ACTION_LABELS = {
    register: "注册",
    unregister: "取消注册",
    start: "启动",
    stop: "停止",
    restart: "重启",
    delete: "删除",
  };

  const CAPABILITY_LABELS = {
    register: "注册",
    unregister: "取消注册",
    start: "启动",
    stop: "停止",
    restart: "重启",
    status: "状态",
    logs: "日志",
  };

  const STATUS_KEY_LABELS = {
    state: "状态",
    message: "消息",
    pid: "进程 ID",
    exit_code: "退出码",
    observed_at: "观测时间",
    started_at: "启动时间",
    provider: "提供方",
  };

  const labelFrom = (map, value) => map[String(value || "")] || String(value || "");

  const serviceId = (svc) => (svc && svc.id ? String(svc.id) : "");
  const serviceName = (svc) => {
    const id = serviceId(svc);
    return svc && svc.spec && svc.spec.name ? String(svc.spec.name) : id;
  };
  const serviceProvider = (svc) =>
    svc && svc.spec && svc.spec.provider ? String(svc.spec.provider) : "";
  const serviceTags = (svc) =>
    svc && svc.spec && Array.isArray(svc.spec.tags) ? svc.spec.tags.map((t) => String(t)) : [];
  const serviceGroups = (svc) =>
    serviceTags(svc)
      .filter((tag) => tag.startsWith(GROUP_TAG_PREFIX) && tag.length > GROUP_TAG_PREFIX.length)
      .map((tag) => tag.slice(GROUP_TAG_PREFIX.length).trim())
      .filter((name) => name.length > 0);

  const stateChipClass = (stateName) => {
    if (stateName === "running") return "chip--ok";
    if (stateName === "failed") return "chip--bad";
    if (stateName === "starting" || stateName === "stopping") return "chip--warn";
    return "chip--neutral";
  };

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
    quickServicesMeta: $("quickServicesMeta"),
    quickServicesList: $("quickServicesList"),
    quickGroupsMeta: $("quickGroupsMeta"),
    quickGroupsList: $("quickGroupsList"),

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
    groups: [],
    selectedServiceId: null,
    selectedService: null,
    selectedStatus: null,
    selectedLogs: [],
    quickStatuses: {},
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
      throw new ApiError(0, "network_error", e && e.message ? e.message : "网络错误");
    }

    if (res.status === 204) return null;

    const ctype = (res.headers.get("content-type") || "").toLowerCase();
    const isJson = ctype.includes("application/json");
    const body = isJson ? await res.json().catch(() => null) : await res.text().catch(() => "");

    if (!res.ok) {
      if (isJson && body && body.error && body.error.message) {
        throw new ApiError(res.status, body.error.code, body.error.message);
      }
      throw new ApiError(res.status, "http_error", typeof body === "string" ? body : "HTTP 错误");
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
    chip.textContent = state.online ? "在线" : "离线";
    chip.classList.remove("chip--ok", "chip--bad", "chip--neutral");
    chip.classList.add(state.online ? "chip--ok" : "chip--bad");
  };

  const showApiError = (e) => {
    if (e instanceof ApiError) {
      if (e.status === 401) {
        setNotice("error", "未授权：缺少或令牌无效。");
        return;
      }
      if (e.status === 0) {
        setNotice("error", `网络错误：${e.message}`);
        return;
      }
      setNotice("error", `${e.code}: ${e.message}`);
      return;
    }
    setNotice("error", e && e.message ? e.message : "发生未知错误");
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

  const loadGroups = async () => {
    setBusy("groups", true);
    try {
      const out = await apiFetch(`${API_PREFIX}/groups`);
      state.groups = Array.isArray(out) ? out : [];
    } finally {
      setBusy("groups", false);
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

  const sortedServices = () => {
    const services = [...state.services];
    services.sort((a, b) => serviceName(a).toLowerCase().localeCompare(serviceName(b).toLowerCase()));
    return services;
  };

  const getServiceById = (id) => state.services.find((svc) => serviceId(svc) === id) || null;

  const groupedServices = () => {
    if (Array.isArray(state.groups) && state.groups.length) {
      return state.groups
        .map((group) => {
          const name = group && group.name ? String(group.name) : "";
          const services = Array.isArray(group && group.services)
            ? group.services
            : Array.isArray(group && group.service_ids)
              ? group.service_ids.map((id) => getServiceById(String(id))).filter(Boolean)
              : [];
          return [name, services];
        })
        .filter(([name]) => name)
        .sort((a, b) => a[0].toLowerCase().localeCompare(b[0].toLowerCase()));
    }

    const groups = new Map();
    for (const svc of sortedServices()) {
      for (const group of serviceGroups(svc)) {
        if (!groups.has(group)) groups.set(group, []);
        groups.get(group).push(svc);
      }
    }
    return [...groups.entries()].sort((a, b) => a[0].toLowerCase().localeCompare(b[0].toLowerCase()));
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
      chip.textContent = p.detected ? "是" : "否";
      tdDetected.appendChild(chip);
      tr.appendChild(tdDetected);

      const tdCaps = document.createElement("td");
      const caps = Array.isArray(p.capabilities) ? p.capabilities : [];
      tdCaps.textContent = caps.map((c) => labelFrom(CAPABILITY_LABELS, c)).join(", ");
      tr.appendChild(tdCaps);

      const tdNotes = document.createElement("td");
      const parts = [];
      if (p.detect_error) parts.push(`错误：${p.detect_error}`);
      if (p.detect_details) parts.push(String(p.detect_details));
      tdNotes.textContent = parts.join(" | ");
      tr.appendChild(tdNotes);

      tbody.appendChild(tr);
    }
  };

  const renderQuickControls = () => {
    renderGroupControls();

    const list = els.quickServicesList;
    list.textContent = "";
    els.quickServicesMeta.textContent = `${state.services.length} 个服务`;

    const services = sortedServices();
    if (!services.length) {
      const empty = document.createElement("div");
      empty.className = "quickService quickService--empty muted";
      empty.textContent = state.token ? "暂无服务" : "保存令牌后加载服务";
      list.appendChild(empty);
      return;
    }

    for (const svc of services) {
      const id = serviceId(svc);
      const name = serviceName(svc);
      const provider = serviceProvider(svc);
      const status = state.quickStatuses[id];
      const stateName = status && status.state ? String(status.state) : "unknown";
      const caps = providerCapabilities(provider);
      const disabledAll = !state.token || !state.online;

      const row = document.createElement("div");
      row.className = "quickService" + (state.selectedServiceId === id ? " is-active" : "");

      const info = document.createElement("button");
      info.className = "quickService__info";
      info.type = "button";
      info.title = "打开详情";
      info.addEventListener("click", () => selectService(id));

      const title = document.createElement("div");
      title.className = "quickService__title";
      title.textContent = name;
      const sub = document.createElement("div");
      sub.className = "quickService__sub";
      sub.textContent = `${id} • ${provider}`;
      info.appendChild(title);
      info.appendChild(sub);

      const chip = document.createElement("span");
      chip.className = `chip ${stateChipClass(stateName)}`;
      chip.textContent = labelFrom(STATE_LABELS, stateName);

      const actions = document.createElement("div");
      actions.className = "quickService__actions";

      const makeBtn = (action, danger = false) => {
        const btn = document.createElement("button");
        btn.className = danger ? "btn btn--danger btn--small" : "btn btn--small";
        btn.type = "button";
        btn.textContent = labelFrom(ACTION_LABELS, action) || action;
        const supported = action === "delete" || caps.has(action);
        btn.disabled = disabledAll || !supported;
        btn.title = supported ? "" : "当前提供方不支持此能力";
        btn.addEventListener("click", () => quickServiceAction(id, action, btn));
        return btn;
      };

      actions.appendChild(makeBtn("start"));
      actions.appendChild(makeBtn("restart"));
      actions.appendChild(makeBtn("delete", true));

      row.appendChild(info);
      row.appendChild(chip);
      row.appendChild(actions);
      list.appendChild(row);
    }
  };

  const summarizeGroupStatus = (services) => {
    const counts = { running: 0, stopped: 0, failed: 0, unknown: 0 };
    for (const svc of services) {
      const id = serviceId(svc);
      const st = state.quickStatuses[id];
      const stateName = st && st.state ? String(st.state) : "unknown";
      if (Object.prototype.hasOwnProperty.call(counts, stateName)) counts[stateName] += 1;
      else counts.unknown += 1;
    }
    const parts = [];
    if (counts.running) parts.push(`运行中 ${counts.running}`);
    if (counts.stopped) parts.push(`已停止 ${counts.stopped}`);
    if (counts.failed) parts.push(`失败 ${counts.failed}`);
    if (counts.unknown) parts.push(`未知 ${counts.unknown}`);
    return parts.length ? parts.join(" / ") : "未知";
  };

  const renderGroupControls = () => {
    const list = els.quickGroupsList;
    list.textContent = "";

    const groups = groupedServices();
    els.quickGroupsMeta.textContent = groups.length ? `${groups.length} 个组` : "无组";

    if (!groups.length) {
      const empty = document.createElement("div");
      empty.className = "quickGroup quickGroup--empty muted";
      empty.textContent = "给服务添加 group:组名 标签后，会在这里出现组控制。";
      list.appendChild(empty);
      return;
    }

    for (const [groupName, services] of groups) {
      const row = document.createElement("div");
      row.className = "quickGroup";

      const info = document.createElement("div");
      info.className = "quickGroup__info";

      const title = document.createElement("div");
      title.className = "quickGroup__title";
      title.textContent = groupName;

      const sub = document.createElement("div");
      sub.className = "quickGroup__sub";
      sub.textContent = `${services.length} 个服务 • ${summarizeGroupStatus(services)}`;

      info.appendChild(title);
      info.appendChild(sub);

      const actions = document.createElement("div");
      actions.className = "quickGroup__actions";

      const makeBtn = (action) => {
        const btn = document.createElement("button");
        btn.className = "btn btn--small";
        btn.type = "button";
        btn.textContent = labelFrom(ACTION_LABELS, action) || action;
        btn.disabled = !state.token || !state.online || services.length === 0;
        btn.addEventListener("click", () => groupServiceAction(groupName, services, action, btn));
        return btn;
      };

      actions.appendChild(makeBtn("start"));
      actions.appendChild(makeBtn("stop"));
      actions.appendChild(makeBtn("restart"));

      row.appendChild(info);
      row.appendChild(actions);
      list.appendChild(row);
    }
  };

  const renderServicesList = () => {
    const q = els.svcSearch.value.trim().toLowerCase();
    const services = sortedServices();

    const filtered = q
      ? services.filter((s) => {
          const name = serviceName(s).toLowerCase();
          const id = serviceId(s).toLowerCase();
          const provider = serviceProvider(s).toLowerCase();
          return name.includes(q) || id.includes(q) || provider.includes(q);
        })
      : services;

    els.servicesList.textContent = "";
    els.servicesMeta.textContent = `显示 ${filtered.length} / 共 ${services.length}`;

    for (const svc of filtered) {
      const id = serviceId(svc);
      const name = serviceName(svc);
      const provider = serviceProvider(svc);

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
      chip.textContent = enabled ? "已启用" : "已停用";
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
    els.svcSubtitle.textContent = `${svc.id} • 提供方=${provider} • 创建=${createdAt} • 更新=${updatedAt}`;

    const st = state.selectedStatus;
    const stateName = st && st.state ? String(st.state) : "unknown";
    els.svcStateChip.textContent = labelFrom(STATE_LABELS, stateName);
    els.svcStateChip.classList.remove("chip--neutral", "chip--ok", "chip--warn", "chip--bad");
    els.svcStateChip.classList.add(stateChipClass(stateName));

    renderStatusKv();
    renderLogs();
    els.svcSpecJson.textContent = JSON.stringify(svc.spec || {}, null, 2);

    // Enable/disable actions based on provider capabilities.
    const caps = providerCapabilities(provider);
    const disableAll = !state.token || !state.online;
    const setBtn = (btn, capOrNull) => {
      const allowed = capOrNull ? caps.has(capOrNull) : true;
      btn.disabled = disableAll || !allowed;
      btn.title = allowed ? "" : "当前提供方不支持此能力";
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
      v.textContent = "尚未加载状态。";
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
      dk.textContent = STATUS_KEY_LABELS[k] || k;
      const dv = document.createElement("div");
      dv.className = "kv__v";
      dv.textContent = k === "state" ? labelFrom(STATE_LABELS, v) : esc(v);
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
      const stream = e && e.stream ? e.stream : "未知";
      const msg = e && e.message ? e.message : "";
      lines.push(`${t} [${stream}] ${msg}`);
    }
    pre.textContent = lines.join("\n");
    pre.scrollTop = pre.scrollHeight;
  };

  const selectService = async (id) => {
    state.selectedServiceId = id;
    renderServicesList();
    renderQuickControls();
    await refreshSelectedService();
  };

  const refreshSelectedService = async () => {
    const id = state.selectedServiceId;
    if (!id) {
      state.selectedService = null;
      state.selectedStatus = null;
      state.selectedLogs = [];
      renderSelectedService();
      renderQuickControls();
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
      renderQuickControls();
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
      state.quickStatuses[id] = st;
      renderSelectedService();
      renderQuickControls();
    } catch (e) {
      showApiError(e);
    }
  };

  const refreshQuickStatus = async (id, { quiet = false } = {}) => {
    const svc = getServiceById(id);
    if (!svc) return null;
    const provider = serviceProvider(svc);
    if (!providerCapabilities(provider).has("status")) return null;

    try {
      const st = await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}/status`);
      state.quickStatuses[id] = st;
      if (state.selectedServiceId === id) {
        state.selectedStatus = st;
        renderSelectedService();
      }
      renderQuickControls();
      return st;
    } catch (e) {
      if (!quiet) showApiError(e);
      return null;
    }
  };

  const refreshQuickStatuses = async () => {
    state.quickStatuses = {};
    await Promise.all(
      state.services.map((svc) => {
        const id = serviceId(svc);
        if (!id) return Promise.resolve(null);
        return refreshQuickStatus(id, { quiet: true });
      }),
    );
    renderQuickControls();
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
        setNotice("info", `${labelFrom(ACTION_LABELS, action)}：完成`);
        await refreshStatus();
      } catch (e) {
        showApiError(e);
      }
    });
  };

  const quickServiceAction = async (id, action, btn) => {
    if (!id) return;
    const svc = getServiceById(id);
    const name = svc ? serviceName(svc) : id;

    if (action === "delete" && !window.confirm(`确定删除服务「${name}」？此操作不可撤销。`)) {
      return;
    }

    await withBusyButton(btn, `quick_${id}_${action}`, async () => {
      try {
        if (action === "delete") {
          await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}`, { method: "DELETE" });
          setNotice("info", `已删除：${name}`);
          if (state.selectedServiceId === id) {
            state.selectedServiceId = null;
            state.selectedService = null;
            state.selectedStatus = null;
            state.selectedLogs = [];
          }
          delete state.quickStatuses[id];
          await loadServices();
          await loadGroups();
          renderServicesList();
          renderSelectedService();
          await refreshQuickStatuses();
          return;
        }

        await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}/${action}`, { method: "POST" });
        setNotice("info", `${name} ${labelFrom(ACTION_LABELS, action)}：完成`);
        await refreshQuickStatus(id);
      } catch (e) {
        showApiError(e);
      }
    });
  };

  const groupServiceAction = async (groupName, services, action, btn) => {
    if (!groupName) return;
    const label = labelFrom(ACTION_LABELS, action) || action;
    if (
      (action === "stop" || action === "restart") &&
      !window.confirm(`确定对服务组「${groupName}」内的 ${services.length} 个服务执行「${label}」？`)
    ) {
      return;
    }

    await withBusyButton(btn, `group_${groupName}_${action}`, async () => {
      try {
        const result = await apiFetch(
          `${API_PREFIX}/groups/${encodeURIComponent(groupName)}/${action}`,
          { method: "POST" },
        );
        const ok = Array.isArray(result && result.succeeded) ? result.succeeded.length : 0;
        const skipped = Array.isArray(result && result.skipped) ? result.skipped.length : 0;
        const failed = Array.isArray(result && result.failed) ? result.failed.length : 0;
        setNotice(
          failed ? "error" : "info",
          `服务组「${groupName}」${label}完成：成功 ${ok}，跳过 ${skipped}，失败 ${failed}`,
        );
        await loadGroups();
        await Promise.all(services.map((svc) => refreshQuickStatus(serviceId(svc), { quiet: true })));
        if (state.selectedServiceId && services.some((svc) => serviceId(svc) === state.selectedServiceId)) {
          await refreshSelectedService();
        }
        renderQuickControls();
      } catch (e) {
        showApiError(e);
      }
    });
  };

  const svcDelete = async () => {
    const id = state.selectedServiceId;
    if (!id) return;
    if (!window.confirm("确定删除此服务？此操作不可撤销。")) return;

    try {
      await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(id)}`, { method: "DELETE" });
      setNotice("info", "已删除");
      state.selectedServiceId = null;
      state.selectedService = null;
      state.selectedStatus = null;
      state.selectedLogs = [];
      await loadServices();
      await loadGroups();
      renderServicesList();
      renderSelectedService();
      await refreshQuickStatuses();
    } catch (e) {
      showApiError(e);
    }
  };

  const openModal = (mode, svcOrNull) => {
    state.editMode = mode;
    state.editId = svcOrNull && svcOrNull.id ? String(svcOrNull.id) : null;
    els.svcModalTitle.textContent = mode === "create" ? "新建服务" : "编辑服务";

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
    interval.placeholder = "间隔（例如 5s）";
    interval.value = hc && hc.interval ? String(hc.interval) : "";

    const timeout = document.createElement("input");
    timeout.className = "input";
    timeout.placeholder = "超时（例如 2s）";
    timeout.value = hc && hc.timeout ? String(hc.timeout) : "";

    const target = document.createElement("input");
    target.className = "input";
    target.placeholder = ty.value === "tcp" ? "地址（host:port）" : "URL（http://...）";
    target.value = hc && (hc.url || hc.address) ? String(hc.url || hc.address) : "";

    const rm = document.createElement("button");
    rm.className = "btn btn--small healthRow__remove";
    rm.type = "button";
    rm.textContent = "移除";
    rm.addEventListener("click", () => row.remove());

    ty.addEventListener("change", () => {
      target.placeholder = ty.value === "tcp" ? "地址（host:port）" : "URL（http://...）";
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
      if (eq <= 0) throw new Error(`环境变量第 ${i + 1} 行：需要 KEY=VALUE`);
      const k = raw.slice(0, eq).trim();
      const v = raw.slice(eq + 1);
      if (!k) throw new Error(`环境变量第 ${i + 1} 行：KEY 不能为空`);
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
    if (!name) throw new Error("名称不能为空");
    if (!validateServiceName(name)) {
      throw new Error("名称必须匹配 ^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$");
    }

    const provider = els.fProvider.value.trim();
    if (!provider) throw new Error("提供方不能为空");

    const cmd = parseCommand(els.fCommand.value);

    const env = parseEnvLines(els.fEnv.value);

    let runtime = {};
    const rtRaw = els.fRuntime.value.trim();
    if (rtRaw) {
      try {
        runtime = JSON.parse(rtRaw);
      } catch (e) {
        throw new Error(`运行时 JSON：${e && e.message ? e.message : "JSON 无效"}`);
      }
      if (runtime == null || Array.isArray(runtime) || typeof runtime !== "object") {
        throw new Error("运行时选项必须是 JSON 对象");
      }
    }

    const mode = els.fRestartMode.value;
    const mr = Number.parseInt(els.fMaxRetries.value, 10);
    if (!Number.isFinite(mr) || mr < 0) throw new Error("最大重试次数必须大于等于 0");

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
      els.formError.textContent = e && e.message ? e.message : "表单无效";
      els.formError.classList.remove("hidden");
      return;
    }

    if (state.editMode === "create") {
      const svc = await apiFetch(`${API_PREFIX}/services`, { method: "POST", json: spec });
      closeModal();
      setNotice("info", "已创建");
      await loadServices();
      await loadGroups();
      renderServicesList();
      await refreshQuickStatuses();
      if (svc && svc.id) await selectService(String(svc.id));
      return;
    }

    if (state.editMode === "edit" && state.editId) {
      await apiFetch(`${API_PREFIX}/services/${encodeURIComponent(state.editId)}`, {
        method: "PUT",
        json: spec,
      });
      closeModal();
      setNotice("info", "已更新");
      await loadServices();
      await loadGroups();
      renderServicesList();
      await refreshQuickStatuses();
      await refreshSelectedService();
      return;
    }

    els.formError.textContent = "编辑模式无效";
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
        setNotice("error", "请先设置令牌以访问 /api/v1 接口。");
        renderQuickControls();
        return;
      }
      try {
        await Promise.all([loadProviders(), loadServices(), loadGroups()]);
        renderProviders();
        renderServicesList();
        renderQuickControls();
        renderSelectedService();
        await refreshQuickStatuses();
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
    renderQuickControls();

    els.btnTokenToggle.addEventListener("click", () => {
      const inp = els.tokenInput;
      const isPw = inp.type === "password";
      inp.type = isPw ? "text" : "password";
      els.btnTokenToggle.textContent = isPw ? "隐藏" : "显示";
    });

    els.btnTokenSave.addEventListener("click", async () => {
      state.token = els.tokenInput.value.trim();
      tokenSave(state.token);
      setNotice("info", state.token ? "令牌已保存" : "令牌已清除");
      await refreshAll();
    });

    els.btnTokenClear.addEventListener("click", async () => {
      els.tokenInput.value = "";
      state.token = "";
      tokenSave("");
      setNotice("info", "令牌已清除");
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
