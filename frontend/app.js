const $ = (id) => document.getElementById(id);
const native = window.__TAURI__;
const invoke = (command, args) => native.core.invoke(command, args);
const titles = {
  installed: "Installed applications",
  leftovers: "Leftover scanner",
  launchers: "GNOME launcher cleanup",
  recovery: "Recovery",
  settings: "Settings",
};
const kindNames = {
  Launcher: "Desktop launcher",
  Icon: "Application icon",
  FlatpakData: "Flatpak data",
  Config: "Configuration",
  Cache: "Cache",
  Data: "Application data",
  State: "Application state",
  AppImage: "AppImage",
  ToolboxBin: "Toolbox program files",
  StandaloneBinary: "Standalone executable",
};
const selectors = [...document.querySelectorAll("[data-view]")];
const workspace = document.querySelector(".workspace");
const systemPreference = matchMedia("(prefers-color-scheme: dark)");
const collator = new Intl.Collator(undefined, {
  sensitivity: "base",
  numeric: true,
});
const state = {
  view: "installed",
  busy: false,
  cancelable: false,
  theme: "system",
  dark: null,
  rpmHelper: false,
  apps: [],
  loaded: false,
  key: "",
  limit: 60,
  snapshots: new Map(),
  recovery: null,
  selection: new Set(),
  icons: new Map(),
  updateRepo: false,
  checkingUpdate: false,
  updateVersion: null,
  restartRequired: false,
};

function node(tag, value, className) {
  const item = document.createElement(tag);
  if (value !== undefined) item.textContent = value;
  if (className) item.className = className;
  return item;
}

function svg(name) {
  const ns = "http://www.w3.org/2000/svg";
  const icon = document.createElementNS(ns, "svg");
  const use = document.createElementNS(ns, "use");
  use.setAttribute("href", "#" + name);
  icon.setAttribute("aria-hidden", "true");
  icon.append(use);
  return icon;
}

function updateIcons(key, data) {
  if (!data?.startsWith("data:image/png;base64,")) return;
  document.querySelectorAll(".app-icon[data-app-key]").forEach((slot) => {
    if (slot.dataset.appKey === key && !slot.querySelector("img")) {
      const image = node("img");
      image.alt = "";
      image.src = data;
      slot.append(image);
    }
  });
}

const pendingIcons = new Set();
let iconTimer = 0;
async function loadIcons() {
  iconTimer = 0;
  const keys = [...pendingIcons].slice(0, 16);
  keys.forEach((key) => pendingIcons.delete(key));
  if (!keys.length) return;
  try {
    const images = await invoke("app_icons", { keys });
    keys.forEach((key) => {
      const data = images[key] ?? null;
      state.icons.set(key, data);
      updateIcons(key, data);
    });
  } catch (error) {
    console.warn("Application icons could not be loaded.", error);
  }
  if (pendingIcons.size && !iconTimer) iconTimer = setTimeout(loadIcons, 16);
}

const iconObserver = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      iconObserver.unobserve(entry.target);
      const key = entry.target.dataset.appKey;
      if (!state.icons.has(key)) pendingIcons.add(key);
    }
    if (pendingIcons.size && !iconTimer) iconTimer = setTimeout(loadIcons, 16);
  },
  { rootMargin: "80px" },
);

function appIcon(app) {
  const icon = node("span", undefined, "app-icon");
  icon.dataset.appKey = app.key;
  icon.append(svg("package-icon"));
  const data = state.icons.get(app.key);
  if (data) {
    const image = node("img");
    image.alt = "";
    image.src = data;
    icon.append(image);
  } else if (!state.icons.has(app.key)) {
    iconObserver.observe(icon);
  }
  return icon;
}

function discardIcons(container) {
  container
    .querySelectorAll(".app-icon")
    .forEach((item) => iconObserver.unobserve(item));
}

function notice(message, error = false) {
  $("notice").hidden = !message;
  $("notice").textContent = message || "";
  $("notice").dataset.error = String(error);
}

function warnings(items, title = "Scan details") {
  $("warnings").hidden = !items.length;
  $("warnings").open = false;
  $("warning-summary").textContent = title + " (" + items.length + ")";
  $("warning-list").replaceChildren(...items.map((item) => node("li", item)));
}

function updateControls() {
  const ready = Boolean(state.updateVersion);
  $("check-updates").hidden = !state.updateRepo || state.restartRequired;
  $("check-updates").disabled = state.busy || state.checkingUpdate;
  $("install-update").hidden = !ready;
  $("update-ready").hidden = !ready || state.view === "settings";
  $("install-update").disabled = $("update-ready").disabled =
    state.busy || state.checkingUpdate;
}

async function checkUpdates(refresh = true) {
  if (!state.updateRepo || state.checkingUpdate || state.restartRequired)
    return;
  state.checkingUpdate = true;
  updateControls();
  $("update-status").textContent = "Checking for updates…";
  try {
    state.updateVersion = await invoke("check_updates", { refresh });
    $("update-status").textContent = state.updateVersion
      ? "Version " + state.updateVersion + " is available."
      : "You’re up to date.";
  } catch (error) {
    state.updateVersion = null;
    $("update-status").textContent = String(error);
  } finally {
    state.checkingUpdate = false;
    updateControls();
  }
}

function reviewUpdate() {
  const version = state.updateVersion;
  if (!version || state.checkingUpdate) return;
  return review(
    "Preparing update…",
    "Install Application Cleanup " + version + "?",
    "Install update",
    () => invoke("preview_update", { version }),
  );
}

function controls() {
  updateControls();
  const scanning = state.busy && state.cancelable;
  $("cancel-scan").hidden = !scanning;
  if (!scanning) $("cancel-scan").disabled = false;
  $("refresh").hidden = state.view === "settings";
  $("refresh").disabled = state.busy;
  $("cleanup-review").disabled = state.busy || !state.selection.size;
  $("theme").disabled = state.busy;
  selectors.forEach((button) => {
    button.disabled = state.busy;
  });
  document.querySelectorAll("[data-operation]").forEach((button) => {
    button.disabled = state.busy;
  });
}

async function work(label, callback, cancelable = false) {
  if (state.busy) return;
  state.busy = true;
  state.cancelable = cancelable;
  controls();
  notice("");
  $("activity").hidden = false;
  $("status").textContent = label;
  $("progress").hidden = true;
  $("progress").textContent = "";
  try {
    return await callback();
  } catch (error) {
    if (String(error) === "Scan canceled.") notice("Scan canceled.");
    else notice(String(error), true);
  } finally {
    state.busy = false;
    state.cancelable = false;
    $("activity").hidden = true;
    controls();
  }
}

function cleanupView() {
  return state.view === "leftovers" || state.view === "launchers";
}

function selectView(view, keyboardFocus = false) {
  if (state.busy) return;
  state.view = view;
  selectors.forEach((button) => {
    if (button.dataset.view === view)
      button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
  $("title").textContent = titles[view];
  ["installed", "cleanup", "recovery", "settings"].forEach((section) => {
    $(section).hidden = section !== (cleanupView() ? "cleanup" : view);
  });
  $("refresh").querySelector("span").textContent = cleanupView()
    ? "Scan again"
    : "Refresh";
  $("cleanup-context").textContent =
    view === "launchers"
      ? "Check desktop entries for missing programs and unavailable icons. Select stale entries to move to Recovery."
      : "Review data and icons associated with removed applications. Check the reason and path before selecting an item.";
  state.selection.clear();
  notice("");
  controls();
  renderView();
  if (keyboardFocus) $("title").focus();
  if (
    (view === "installed" && !state.loaded) ||
    (cleanupView() && !state.snapshots.has(view)) ||
    (view === "recovery" && state.recovery === null)
  )
    refreshView(false);
}

function renderView() {
  const snapshot = state.snapshots.get(state.view);
  warnings(snapshot?.warnings ?? []);
  if (state.view === "installed") {
    renderApps();
    renderDetails();
  } else if (cleanupView()) {
    renderCleanup(snapshot?.items ?? [], Boolean(snapshot));
  } else if (state.view === "recovery") {
    renderRecovery();
  }
}

async function fetchView(view, refresh) {
  if (view === "installed") {
    const scanned = await invoke("installed", { refresh });
    if (refresh) state.snapshots.clear();
    state.snapshots.set(view, scanned);
    state.apps = scanned.items;
    state.loaded = true;
    if (!state.apps.some((app) => app.key === state.key)) closeDetails(false);
  } else if (view === "leftovers" || view === "launchers") {
    const scanned = await invoke("scan_cleanup", {
      launchers: view === "launchers",
      refresh,
    });
    if (refresh) {
      state.snapshots.clear();
      state.loaded = false;
    }
    state.selection.clear();
    state.snapshots.set(view, scanned);
  } else if (view === "recovery") {
    state.recovery = await invoke("recovery");
  }
  renderView();
}

function refreshView(refresh) {
  const view = state.view;
  const scanning = view !== "recovery";
  const label =
    view === "installed"
      ? "Finding installed applications…"
      : view === "recovery"
        ? "Loading Recovery…"
        : "Scanning for candidates…";
  return work(label, () => fetchView(view, refresh), scanning);
}

function positionPopover(menu, x, y) {
  const { width, height } = menu.getBoundingClientRect();
  menu.style.left = Math.max(8, Math.min(x, innerWidth - width - 8)) + "px";
  menu.style.top = Math.max(8, Math.min(y, innerHeight - height - 8)) + "px";
}

let rowTarget = null;
function showRowMenu(event, app, button) {
  event.preventDefault();
  if (state.busy) return;
  rowTarget = { app, button };
  $("row-uninstall").hidden = !app.removable;
  $("row-remove-launcher").hidden = !app.launcher_removable;
  const bounds = button.getBoundingClientRect();
  const menu = $("row-menu");
  if (!menu.matches(":popover-open")) menu.showPopover();
  positionPopover(
    menu,
    event.clientX || bounds.left + 12,
    event.clientY || bounds.bottom,
  );
  $("row-details").focus({ preventScroll: true });
}

function reviewUninstall(app) {
  return review(
    "Preparing uninstall preview…",
    "Uninstall " + app.name + "?",
    "Uninstall",
    () => invoke("preview_uninstall", { key: app.key }),
    app.source !== "RPM" || state.rpmHelper,
  );
}

function reviewLauncherRemoval(app) {
  return review(
    "Checking launcher…",
    "Remove the launcher for " + app.name + "?",
    "Remove launcher",
    () => invoke("preview_remove_launcher", { key: app.key }),
  );
}

function renderApps() {
  if ($("row-menu").matches(":popover-open")) $("row-menu").hidePopover();
  const results = $("app-list");
  discardIcons(results);
  const query = $("search").value.trim().toLocaleLowerCase();
  const source = $("source").value;
  const bySource = $("sort").value === "source";
  const filtered = state.apps
    .filter(
      (app) =>
        (!source || source === app.source) &&
        (!query ||
          [app.name, app.id, app.source, app.origin].some((value) =>
            value.toLocaleLowerCase().includes(query),
          )),
    )
    .sort(
      (a, b) =>
        (bySource ? collator.compare(a.source, b.source) : 0) ||
        collator.compare(a.name, b.name) ||
        collator.compare(a.source, b.source),
    );
  const visible = filtered.slice(0, state.limit);
  results.replaceChildren(
    ...visible.map((app) => {
      const row = node("li", undefined, "app-row");
      const button = node("button");
      button.type = "button";
      button.dataset.key = app.key;
      button.setAttribute("aria-pressed", String(state.key === app.key));
      const description = node("span", undefined, "app-text");
      description.append(node("strong", app.name), node("span", app.source));
      button.append(appIcon(app), description);
      button.addEventListener("click", () => openDetails(app.key));
      button.addEventListener("contextmenu", (event) =>
        showRowMenu(event, app, button),
      );
      button.addEventListener("keydown", (event) => {
        if (
          event.key === "ContextMenu" ||
          (event.shiftKey && event.key === "F10")
        )
          showRowMenu(event, app, button);
      });
      row.append(button);
      return row;
    }),
  );
  $("app-count").textContent = state.loaded
    ? filtered.length +
      (filtered.length === 1 ? " application" : " applications")
    : "";
  $("app-empty").hidden = !state.loaded || filtered.length > 0;
  $("app-empty").textContent = state.apps.length
    ? "No applications match your search."
    : "No installed applications were found.";
  $("show-more").hidden = visible.length >= filtered.length;
}

function openDetails(key) {
  state.key = key;
  workspace.dataset.detail = "true";
  $("app-list")
    .querySelectorAll("button")
    .forEach((button) =>
      button.setAttribute("aria-pressed", String(button.dataset.key === key)),
    );
  renderDetails();
  if (matchMedia("(max-width: 670px)").matches) $("back").focus();
}

function closeDetails(restoreFocus) {
  const key = state.key;
  state.key = "";
  workspace.dataset.detail = "false";
  $("app-list")
    .querySelectorAll("button")
    .forEach((button) => button.setAttribute("aria-pressed", "false"));
  renderDetails();
  if (!restoreFocus) return;
  const selected = [...$("app-list").querySelectorAll("button")].find(
    (button) => button.dataset.key === key,
  );
  (selected || $("search")).focus();
}

function renderDetails() {
  const container = $("app-detail-content");
  discardIcons(container);
  const app = state.apps.find((app) => app.key === state.key);
  if (!app) {
    container.replaceChildren(
      node("p", "Select an application to see its details.", "empty"),
    );
    return;
  }
  const title = node("div", undefined, "detail-title");
  const name = node("h2", app.name);
  name.id = "detail-name";
  name.tabIndex = -1;
  title.append(appIcon(app), name);
  container.replaceChildren(title);
  if (app.summary) container.append(node("p", app.summary, "detail-summary"));
  const fields = node("dl", undefined, "detail-fields");
  for (const [label, value] of [
    ["Source", app.source],
    ["Version", app.version],
    ["Application ID", app.id],
    ["Installation", app.origin],
    ["Launcher", app.launcher],
  ]) {
    if (value) fields.append(node("dt", label), node("dd", value));
  }
  container.append(fields);
  if (app.removable) {
    const remove = node("button", "Review uninstall");
    remove.type = "button";
    remove.dataset.operation = "";
    remove.disabled = state.busy;
    remove.addEventListener("click", () => reviewUninstall(app));
    container.append(remove);
  } else {
    const hint =
      app.source === "Manual" && app.launcher_removable
        ? "You can remove this launcher from the app menu."
        : app.source === "Manual"
          ? "Use the installation’s original removal method."
          : app.source === "RPM"
            ? "This package is protected or DNF5 is unavailable."
            : "This AppImage is outside the supported removal locations.";
    container.append(node("p", hint, "detail-hint"));
  }
  if (app.launcher_removable) {
    const remove = node("button", "Remove launcher…", "remove-launcher");
    remove.type = "button";
    remove.dataset.operation = "";
    remove.disabled = state.busy;
    remove.addEventListener("click", () => reviewLauncherRemoval(app));
    container.append(remove);
  }
}

function bytes(value) {
  if (value === null) return "Size unavailable";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return (
    new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(
      value,
    ) +
    " " +
    units[unit]
  );
}

function renderCleanup(items, loaded) {
  $("cleanup-count").textContent = loaded
    ? items.length + (items.length === 1 ? " candidate" : " candidates")
    : "";
  $("cleanup-empty").hidden = !loaded || items.length > 0;
  $("cleanup-empty").textContent =
    state.view === "launchers"
      ? "No launcher issues were found."
      : "No attributable leftovers were found.";
  $("cleanup-rows").replaceChildren(
    ...items.map((item, index) => {
      const row = node("li", undefined, "row");
      const label = node(
        "label",
        kindNames[item.kind] || item.kind,
        "row-title",
      );
      const path = node("span", item.path, "row-path");
      const status = item.selectable
        ? item.confirmed
          ? "Confirmed"
          : "Needs review"
        : "Read-only";
      const details = node(
        "p",
        [status, item.file_type, bytes(item.bytes)].join(" · "),
        "row-meta",
      );
      const content = node("div", undefined, "row-content");
      if (item.selectable) {
        const checkbox = node("input");
        checkbox.id = "candidate-" + index;
        checkbox.type = "checkbox";
        checkbox.dataset.operation = "";
        checkbox.checked = state.selection.has(item.path);
        label.htmlFor = checkbox.id;
        path.id = "candidate-path-" + index;
        checkbox.setAttribute("aria-describedby", path.id);
        checkbox.addEventListener("change", () => {
          if (checkbox.checked) state.selection.add(item.path);
          else state.selection.delete(item.path);
          controls();
        });
        row.append(checkbox);
      } else {
        row.append(svg(item.kind === "Launcher" ? "launcher-icon" : "files"));
      }
      content.append(label, path, details, node("p", item.reason, "reason"));
      row.append(content);
      return row;
    }),
  );
  controls();
}

function renderRecovery() {
  const rows = state.recovery ?? [];
  $("recovery-empty").hidden = state.recovery === null || rows.length > 0;
  $("recovery-rows").replaceChildren(
    ...rows.map((item) => {
      const row = node("li", undefined, "row");
      const content = node("div", undefined, "row-content");
      content.append(
        node("span", kindNames[item.kind] || item.kind, "row-title"),
        node("span", item.path, "row-path"),
      );
      const restore = node("button", "Restore");
      restore.type = "button";
      restore.dataset.operation = "";
      restore.setAttribute("aria-label", "Restore " + item.path);
      restore.addEventListener("click", () =>
        work("Restoring item…", async () => {
          const result = await invoke("restore", { id: item.id });
          await afterOperation(result, "Restored to its original location.");
        }),
      );
      const remove = node("button", "Delete permanently");
      remove.type = "button";
      remove.dataset.operation = "";
      remove.setAttribute("aria-label", "Delete permanently " + item.path);
      remove.addEventListener("click", () =>
        review(
          "Checking Recovery entry…",
          "Permanently delete this item?",
          "Delete permanently",
          () => invoke("preview_delete", { id: item.id }),
        ),
      );
      const actions = node("div", undefined, "recovery-actions");
      actions.append(restore, remove);
      row.append(content, actions);
      return row;
    }),
  );
}

async function review(label, title, action, request, canRun = true) {
  const result = await work(label, request);
  if (!result) return;
  const dialog = $("confirmation");
  $("confirmation-title").textContent = title;
  $("confirmation-description").textContent = result.explanation;
  $("confirmation-items").replaceChildren(
    ...result.items.map((item) => node("li", item)),
  );
  $("confirmation-authorization").hidden = !result.authorization;
  $("confirmation-authorization").textContent = result.authorization ?? "";
  $("confirm").textContent = action;
  $("confirm").disabled = !canRun;
  dialog.returnValue = "";
  dialog.showModal();
  dialog.addEventListener(
    "close",
    () => {
      if (dialog.returnValue !== "confirm" || !canRun) return;
      const deleting = action === "Delete permanently";
      const uninstalling = action === "Uninstall";
      const launcherOnly = action === "Remove launcher";
      const updating = action === "Install update";
      const label = updating
        ? "Installing update…"
        : deleting
          ? "Deleting from Recovery…"
          : uninstalling
            ? "Uninstall in progress…"
            : "Moving selected entries…";
      work(label, async () => {
        const report = await invoke("confirm", { token: result.token });
        if (updating) {
          state.updateVersion = null;
          state.restartRequired = true;
          $("update-status").textContent =
            "Update installed. Close and reopen Application Cleanup.";
          notice($("update-status").textContent);
          return;
        }
        let message = "No selected items were moved.";
        if (deleting) message = "Item permanently deleted.";
        else if (uninstalling) message = "Application uninstalled.";
        else if (launcherOnly) message = "Launcher moved to Recovery.";
        else if (report.completed) {
          message =
            report.completed +
            (report.completed === 1
              ? " item moved to Recovery."
              : " items moved to Recovery.");
        }
        await afterOperation(report, message);
      });
    },
    { once: true },
  );
}

async function afterOperation(report, message) {
  state.loaded = false;
  state.snapshots.clear();
  state.recovery = null;
  state.selection.clear();
  const details = [...report.warnings];
  try {
    await fetchView(state.view, false);
    details.push(...(state.snapshots.get(state.view)?.warnings ?? []));
  } catch (error) {
    details.push("Could not refresh the view. " + error);
  }
  notice(message, !report.completed);
  warnings(details, "Operation details");
}

function applyAppearance() {
  document.documentElement.dataset.theme =
    state.theme === "system"
      ? (state.dark ?? systemPreference.matches)
        ? "dark"
        : "light"
      : state.theme;
}

$("app-menu").addEventListener("toggle", () => {
  const menu = $("app-menu");
  const open = menu.matches(":popover-open");
  const trigger = $("app-menu-button");
  trigger.setAttribute("aria-expanded", String(open));
  if (open) {
    const bounds = trigger.getBoundingClientRect();
    positionPopover(menu, bounds.left, bounds.bottom + 4);
    $("show-about").focus({ preventScroll: true });
  }
});
$("show-about").addEventListener("click", () => {
  $("app-menu").hidePopover();
  $("app-menu-button").focus();
  $("about").showModal();
});
document.addEventListener("pointerdown", (event) => {
  const menu = $("row-menu");
  if (menu.matches(":popover-open") && !menu.contains(event.target))
    menu.hidePopover();
});
$("row-menu").addEventListener("focusout", (event) => {
  const menu = $("row-menu");
  if (menu.matches(":popover-open") && !menu.contains(event.relatedTarget))
    menu.hidePopover();
});
$("row-details").addEventListener("click", () => {
  $("row-menu").hidePopover();
  if (!rowTarget || state.busy) return;
  rowTarget.button.focus();
  openDetails(rowTarget.app.key);
});
$("row-uninstall").addEventListener("click", () => {
  $("row-menu").hidePopover();
  if (!rowTarget?.app.removable || state.busy) return;
  rowTarget.button.focus();
  reviewUninstall(rowTarget.app);
});
$("row-remove-launcher").addEventListener("click", () => {
  $("row-menu").hidePopover();
  if (!rowTarget?.app.launcher_removable || state.busy) return;
  rowTarget.button.focus();
  reviewLauncherRemoval(rowTarget.app);
});
for (const [menuId, trigger] of [
  ["app-menu", () => $("app-menu-button")],
  ["row-menu", () => rowTarget?.button],
]) {
  $(menuId).addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    $(menuId).hidePopover();
    trigger()?.focus();
  });
}

selectors.forEach((button) =>
  button.addEventListener("click", () => selectView(button.dataset.view, true)),
);
$("check-updates").addEventListener("click", () => checkUpdates());
$("install-update").addEventListener("click", reviewUpdate);
$("update-ready").addEventListener("click", reviewUpdate);
$("refresh").addEventListener("click", () => refreshView(true));
$("cancel-scan").addEventListener("click", async () => {
  $("cancel-scan").disabled = true;
  try {
    await invoke("cancel_scan");
    if (state.busy) $("status").textContent = "Canceling scan…";
  } catch (error) {
    notice(String(error), true);
  }
});
["search", "source", "sort"].forEach((id) =>
  $(id).addEventListener("input", () => {
    state.limit = 60;
    renderApps();
  }),
);
$("show-more").addEventListener("click", () => {
  const index = state.limit;
  state.limit += 60;
  renderApps();
  $("app-list").querySelectorAll("button")[index]?.focus();
});
$("back").addEventListener("click", () => closeDetails(true));
$("cleanup-review").addEventListener("click", () =>
  review(
    "Checking selected entries…",
    "Move selected entries to Recovery?",
    "Move to Recovery",
    () =>
      invoke("preview_cleanup", {
        paths: [...state.selection],
        launchers: state.view === "launchers",
      }),
  ),
);
$("theme").addEventListener("change", async () => {
  const old = state.theme;
  const selected = $("theme").value;
  state.theme = selected;
  applyAppearance();
  await work("Saving appearance…", async () => {
    try {
      await invoke("save_theme", { theme: selected });
    } catch (error) {
      state.theme = old;
      $("theme").value = old;
      applyAppearance();
      throw error;
    }
  });
});
systemPreference.addEventListener("change", applyAppearance);

async function start() {
  let configurationWarning = "";
  applyAppearance();
  try {
    await native.event.listen("system-theme", ({ payload }) => {
      state.dark = payload;
      applyAppearance();
    });
    await native.event.listen("operation-progress", ({ payload }) => {
      if (!state.busy) return;
      $("progress").hidden = false;
      $("progress").textContent = payload;
    });
    const settings = await invoke("configuration");
    state.theme = settings.theme;
    state.dark = settings.dark;
    state.rpmHelper = settings.rpm_helper;
    state.updateRepo = settings.update_repo;
    $("about-version").textContent = "Version " + settings.version;
    if (settings.about_icon?.startsWith("data:image/png;base64,")) {
      $("about-icon").src = settings.about_icon;
      $("about-icon").hidden = false;
    }
    configurationWarning = settings.warning ?? "";
    $("theme").value = state.theme;
    $("rpm-helper").textContent = state.rpmHelper
      ? "Available"
      : "Install or update the Fedora RPM";
    applyAppearance();
  } catch (error) {
    configurationWarning = "Could not load application settings. " + error;
  }
  try {
    await invoke("show_window");
  } catch (error) {
    notice(String(error), true);
  }
  selectView("installed");
  if (state.updateRepo) checkUpdates(false);
  else
    $("update-status").textContent =
      "Install the signed Fedora release to enable updates.";
  if (configurationWarning) notice(configurationWarning, true);
}

start();
