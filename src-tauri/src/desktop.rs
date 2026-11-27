use crate::{
    cleanup::{self, Candidate, History, Kind, Recovery},
    inventory::{self, Application, Install, Inventory, Locations},
    packages::{self, Preview},
    updates,
};
use base64::Engine;
use gtk::{gio, prelude::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    theme: Theme,
}

#[derive(Serialize)]
struct Configuration {
    theme: Theme,
    dark: Option<bool>,
    warning: Option<String>,
    rpm_helper: bool,
    update_repo: bool,
    version: &'static str,
    about_icon: String,
}

#[derive(Default)]
struct Session {
    inventory: Option<Arc<Inventory>>,
    launchers: Vec<Candidate>,
    leftovers: Vec<Candidate>,
    recent: Option<BTreeSet<String>>,
    recent_leftovers: Vec<Candidate>,
    pending: Option<(u64, Instant, Action)>,
}

struct Core {
    locations: Locations,
    working: AtomicBool,
    scanning: AtomicBool,
    canceled: AtomicBool,
    next_token: AtomicU64,
    session: Mutex<Session>,
}

impl Core {
    fn new(locations: Locations) -> Self {
        Self {
            locations,
            working: false.into(),
            scanning: false.into(),
            canceled: false.into(),
            next_token: 1.into(),
            session: Mutex::new(Session::default()),
        }
    }

    fn session(&self) -> Result<MutexGuard<'_, Session>, String> {
        self.session
            .lock()
            .map_err(|_| "The current session could not be read.".into())
    }

    fn begin(&self, scan: bool) -> Result<Work<'_>, String> {
        if self.working.swap(true, Ordering::AcqRel) {
            return Err("An operation is already running.".into());
        }
        self.canceled.store(false, Ordering::Release);
        self.scanning.store(scan, Ordering::Release);
        Ok(Work(self))
    }

    fn settings_path(&self) -> PathBuf {
        self.locations
            .config
            .join("application-cleanup/settings.json")
    }

    fn history_path(&self) -> PathBuf {
        self.locations
            .state
            .join("application-cleanup/history.json")
    }

    fn inventory(&self, refresh: bool) -> Result<Arc<Inventory>, String> {
        if !refresh && let Some(inventory) = &self.session()?.inventory {
            return Ok(inventory.clone());
        }
        let value = Arc::new(inventory::scan(&self.locations, &self.canceled)?);
        self.session()?.inventory = Some(value.clone());
        Ok(value)
    }

    fn store_preview(&self, preview: Preview, action: Action) -> Result<Confirmation, String> {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.session()?.pending = Some((token, Instant::now(), action));
        Ok(Confirmation { token, preview })
    }

    fn invalidate(&self) -> Result<(), String> {
        let mut session = self.session()?;
        *session = Session {
            recent: session.recent.take(),
            ..Session::default()
        };
        Ok(())
    }
}

struct Work<'a>(&'a Core);
impl Drop for Work<'_> {
    fn drop(&mut self) {
        self.0.scanning.store(false, Ordering::Release);
        self.0.working.store(false, Ordering::Release);
    }
}

enum Action {
    Uninstall {
        app: Box<Application>,
        items: Vec<String>,
        files: Vec<Candidate>,
    },
    Cleanup {
        selections: Vec<Candidate>,
        recent: bool,
        permanent: bool,
    },
    RemoveLauncher(Box<Candidate>),
    Update(String),
    Delete(Recovery),
}

#[derive(Serialize)]
struct Confirmation {
    token: u64,
    #[serde(flatten)]
    preview: Preview,
}

#[derive(Serialize)]
struct Scan<T> {
    items: T,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ResultReport {
    completed: usize,
    warnings: Vec<String>,
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())?
}

fn on_main<T: Send + 'static>(
    app: &AppHandle,
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        // The window can close while a worker is waiting for GTK.
        let _ = tx.send(job());
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "The desktop environment did not respond.".into())
}

fn theme_settings() -> Option<gio::Settings> {
    let schema =
        gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.interface", true)?;
    schema
        .has_key("color-scheme")
        .then(|| gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None))
}

fn system_dark(settings: &gio::Settings) -> Option<bool> {
    match settings.string("color-scheme").as_str() {
        "prefer-dark" => Some(true),
        "prefer-light" => Some(false),
        _ => None,
    }
}

#[tauri::command]
async fn configuration(
    app: AppHandle,
    state: State<'_, Arc<Core>>,
) -> Result<Configuration, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let dark = on_main(&app, || theme_settings().as_ref().and_then(system_dark))?;
        let (settings, warning) = match cleanup::read_json::<Settings>(&core.settings_path()) {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(format!("Could not read saved appearance. {error}")),
            ),
        };
        Ok(Configuration {
            theme: settings.theme,
            dark,
            warning,
            rpm_helper: packages::rpm_helper_available(),
            update_repo: updates::available(),
            version: env!("CARGO_PKG_VERSION"),
            about_icon: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD
                    .encode(include_bytes!("../icons/128x128.png"))
            ),
        })
    })
    .await
}

#[tauri::command]
fn show_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_theme(theme: Theme, state: State<'_, Arc<Core>>) -> Result<(), String> {
    let core = Arc::clone(&state);
    blocking(move || cleanup::write_json(&core.settings_path(), &Settings { theme })).await
}

#[tauri::command]
fn cancel_scan(state: State<'_, Arc<Core>>) {
    if state.scanning.load(Ordering::Acquire) {
        state.canceled.store(true, Ordering::Release);
    }
}

#[tauri::command]
async fn installed(
    refresh: bool,
    state: State<'_, Arc<Core>>,
) -> Result<Scan<Vec<Application>>, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(true)?;
        if refresh {
            core.invalidate()?;
        }
        let inventory = core.inventory(refresh)?;
        Ok(Scan {
            items: inventory.applications.clone(),
            warnings: inventory
                .warnings
                .iter()
                .chain(&inventory.managed_warnings)
                .cloned()
                .collect(),
        })
    })
    .await
}

fn valid_icon(value: &str) -> bool {
    !value.is_empty() && value.len() < 4096 && !value.chars().any(char::is_control)
}

fn icon_available(value: &str, theme: Option<&gtk::IconTheme>) -> bool {
    let path = Path::new(value);
    if !valid_icon(value) {
        return false;
    }
    if path.is_absolute() {
        return std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
            && gtk::gdk_pixbuf::Pixbuf::file_info(path).is_some();
    }
    !value.contains('/') && theme.is_none_or(|theme| theme.has_icon(value))
}

fn icon_data(value: &str, theme: Option<&gtk::IconTheme>) -> Option<String> {
    if !valid_icon(value) {
        return None;
    }
    let path = Path::new(value);
    let pixbuf = if path.is_absolute() {
        if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 2 * 1024 * 1024) {
            return None;
        }
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, 64, 64, true).ok()?
    } else {
        if value.contains('/') {
            return None;
        }
        theme?
            .load_icon(value, 64, gtk::IconLookupFlags::FORCE_SIZE)
            .ok()??
    };
    let png = pixbuf.save_to_bufferv("png", &[]).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

#[tauri::command]
async fn app_icons(
    app: AppHandle,
    keys: Vec<String>,
    state: State<'_, Arc<Core>>,
) -> Result<BTreeMap<String, String>, String> {
    if keys.len() > 24 {
        return Err("Too many icons requested.".into());
    }
    let core = Arc::clone(&state);
    blocking(move || {
        let icons: Vec<_> = core
            .session()?
            .inventory
            .as_ref()
            .map(|inventory| {
                keys.iter()
                    .filter_map(|key| {
                        let app = inventory.applications.iter().find(|app| &app.key == key)?;
                        Some((app.key.clone(), app.icon.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        on_main(&app, move || {
            let theme = gtk::IconTheme::default();
            icons
                .into_iter()
                .filter_map(|(key, icon)| Some((key, icon_data(&icon, theme.as_ref())?)))
                .collect()
        })
    })
    .await
}

fn add_icon_diagnostics(
    app: &AppHandle,
    core: &Core,
    inventory: &Inventory,
    rows: &mut Vec<Candidate>,
) -> Result<(), String> {
    let launchers: Vec<_> = inventory
        .launchers
        .iter()
        .filter(|entry| !entry.hidden && !entry.conditional)
        .filter(|entry| !entry.icon.is_empty())
        .map(|entry| (entry.path.clone(), entry.icon.clone()))
        .collect();
    let issues = on_main(app, move || {
        let theme = gtk::IconTheme::default();
        launchers
            .into_iter()
            .filter(|(_, icon)| !icon_available(icon, theme.as_ref()))
            .collect::<Vec<_>>()
    })?;
    for (path, icon) in issues {
        inventory::check_cancel(&core.canceled)?;
        let reason = format!(
            "The launcher icon is missing or unavailable in the current icon theme: {icon}."
        );
        if let Some(row) = rows.iter_mut().find(|row| row.path == path) {
            row.reason.push(' ');
            row.reason.push_str(&reason);
        } else {
            rows.push(cleanup::icon_diagnostic(
                &core.locations,
                path,
                reason,
                &core.canceled,
            ));
        }
    }
    Ok(())
}

#[tauri::command]
async fn scan_cleanup(
    app: AppHandle,
    launchers: bool,
    refresh: bool,
    recent: Option<bool>,
    state: State<'_, Arc<Core>>,
) -> Result<Scan<Vec<Candidate>>, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(true)?;
        if refresh {
            core.invalidate()?;
        }
        let inventory = core.inventory(refresh)?;
        let mut warnings = inventory.warnings.clone();
        let recent = recent.unwrap_or(false);
        if launchers && recent {
            return Err("Choose a single scan.".into());
        }
        let items = if launchers {
            let mut items = cleanup::scan_launchers(&core.locations, &inventory, &core.canceled)?;
            add_icon_diagnostics(&app, &core, &inventory, &mut items)?;
            core.session()?.launchers = items.clone();
            items
        } else {
            let items = scan_leftovers(&core, &inventory, recent, &mut warnings)?;
            let mut session = core.session()?;
            if recent {
                session.recent_leftovers = items.clone();
            } else {
                session.leftovers = items.clone();
            }
            items
        };
        Ok(Scan { items, warnings })
    })
    .await
}

fn load_history(core: &Core, warnings: &mut Vec<String>) -> History {
    match History::read(&core.history_path()) {
        Ok(history) => history,
        Err(error) => {
            warnings.push(error);
            History::default()
        }
    }
}

fn scan_leftovers(
    core: &Core,
    inventory: &Inventory,
    recent: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Candidate>, String> {
    let removed_ids = if recent {
        core.session()?
            .recent
            .clone()
            .ok_or("Uninstall an app before scanning its leftovers.")?
    } else {
        load_history(core, warnings).removed_ids
    };
    let (items, extra) = cleanup::scan_leftovers(
        &core.locations,
        inventory,
        &removed_ids,
        recent,
        &core.canceled,
    )?;
    warnings.extend(extra);
    Ok(items)
}

fn recent_candidate(core: &Core, path: &Path) -> Result<Candidate, String> {
    core.session()?
        .recent_leftovers
        .iter()
        .find(|candidate| candidate.path == path && candidate.selectable)
        .cloned()
        .ok_or_else(|| "This entry is no longer available. Scan again.".into())
}

#[tauri::command]
async fn leftover_contents(
    path: PathBuf,
    folders: Vec<String>,
    state: State<'_, Arc<Core>>,
) -> Result<cleanup::LeftoverContents, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let candidate = recent_candidate(&core, &path)?;
        cleanup::leftover_contents(&core.locations, &candidate, &folders)
    })
    .await
}

#[tauri::command]
async fn open_leftover(
    app: AppHandle,
    path: PathBuf,
    state: State<'_, Arc<Core>>,
) -> Result<(), String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let candidate = recent_candidate(&core, &path)?;
        cleanup::revalidate(&core.locations, &candidate)?;
        let folder = if candidate.file_type == "Folder" {
            candidate.path
        } else {
            candidate
                .path
                .parent()
                .ok_or("No containing folder.")?
                .to_owned()
        };
        cleanup::secure_dir(&folder)?;
        on_main(&app, move || {
            let manager = gio::AppInfo::default_for_type("inode/directory", false)
                .ok_or("No default file manager was found.")?;
            manager
                .launch(&[gio::File::for_path(folder)], gio::AppLaunchContext::NONE)
                .map_err(|error| format!("Could not open the file manager: {error}"))
        })?
    })
    .await
}

#[tauri::command]
async fn recovery(state: State<'_, Arc<Core>>) -> Result<Vec<Recovery>, String> {
    let core = Arc::clone(&state);
    blocking(move || cleanup::list_recovery(&core.locations)).await
}

#[tauri::command]
async fn uninstall_history(
    state: State<'_, Arc<Core>>,
) -> Result<cleanup::UninstallHistory, String> {
    let path = state.history_path();
    blocking(move || Ok(History::read(&path)?.recent_first())).await
}

#[tauri::command]
async fn restore(id: String, state: State<'_, Arc<Core>>) -> Result<ResultReport, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let record = cleanup::restore(&core.locations, &id)?;
        core.invalidate()?;
        let mut warnings = Vec::new();
        if record.kind == Kind::Launcher
            && let Err(error) = cleanup::refresh_launchers(&core.locations)
        {
            warnings.push(error);
        }
        Ok(ResultReport {
            completed: 1,
            warnings,
        })
    })
    .await
}

#[tauri::command]
async fn preview_uninstall(
    key: String,
    state: State<'_, Arc<Core>>,
) -> Result<Confirmation, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let inventory = core.inventory(false)?;
        let app = inventory.applications.iter().find(|app| app.key == key && app.removable)
            .ok_or("This application has no supported removal action.")?.clone();
        let mut files = Vec::new();
        let preview = match &app.install {
            Install::Rpm(spec) => {
                let mut preview = packages::preview_rpm(spec)?;
                if !packages::rpm_helper_available() {
                    preview.explanation.push_str(" Install or update the Fedora RPM to enable removal.");
                }
                preview
            }
            Install::Flatpak { reference, scope } => packages::preview_flatpak(reference, scope)?,
            Install::Managed { manager, .. } => manager.preview(&app)?,
            Install::AppImage(_) => {
                files = cleanup::appimage_candidates(&core.locations, &app, &inventory)?;
                let items = files.iter().map(|candidate| candidate.path.display().to_string()).collect();
                Preview { items, explanation: "The AppImage and any listed user launchers will move to Recovery. Application data stays in place.".into(), authorization: None }
            }
            Install::Toolbox(bin) => {
                files = cleanup::toolbox_candidates(&core.locations, bin)?;
                let items = files.iter().map(|candidate| candidate.path.display().to_string()).collect();
                Preview { items, explanation: "Toolbox program files and matching launchers will move to Recovery. Exit Toolbox first. Installed IDEs and Toolbox data stay in place.".into(), authorization: None }
            }
            Install::Standalone(path) => {
                let desktop = app.launcher.as_deref().ok_or("The standalone app launcher is unavailable.")?;
                files = cleanup::standalone_candidates(&core.locations, path, desktop, &inventory)?;
                let items = files.iter().map(|candidate| candidate.path.display().to_string()).collect();
                Preview { items, explanation: "The executable and its launcher will move to Recovery. Quit the app first. App data stays in place.".into(), authorization: None }
            }
            Install::Manual => return Err("This manual installation has no verified uninstall record.".into()),
        };
        core.store_preview(Preview { items: preview.items.clone(), ..preview },
            Action::Uninstall { app: Box::new(app), items: preview.items, files })
    }).await
}

#[tauri::command]
async fn check_updates(refresh: bool) -> Result<Option<String>, String> {
    blocking(move || updates::check(refresh)).await
}

#[tauri::command]
async fn preview_update(
    version: String,
    state: State<'_, Arc<Core>>,
) -> Result<Confirmation, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let preview = updates::preview(&version)?;
        core.store_preview(preview, Action::Update(version))
    })
    .await
}

#[tauri::command]
async fn preview_remove_launcher(
    key: String,
    state: State<'_, Arc<Core>>,
) -> Result<Confirmation, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let inventory = core.inventory(false)?;
        let launcher = inventory.applications.iter()
            .find(|app| app.key == key && app.launcher_removable)
            .and_then(|app| app.launcher.as_deref())
            .ok_or("This application has no removable user launcher.")?;
        let candidate = cleanup::manual_launcher_candidate(&core.locations, launcher)?;
        let preview = Preview {
            items: vec![candidate.path.display().to_string()],
            explanation: "This moves the shortcut to Recovery and removes it from the app menu. The program and its data stay in place.".into(),
            authorization: None,
        };
        core.store_preview(preview, Action::RemoveLauncher(Box::new(candidate)))
    }).await
}

#[tauri::command]
async fn preview_delete(id: String, state: State<'_, Arc<Core>>) -> Result<Confirmation, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let record = cleanup::permanent_preview(&core.locations, &id)?;
        let preview = Preview {
            items: vec![record.path.display().to_string()],
            explanation: "This permanently deletes the selected item and its contents from Recovery. You cannot undo this action.".into(),
            authorization: None,
        };
        core.store_preview(preview, Action::Delete(record))
    }).await
}

#[tauri::command]
async fn preview_cleanup(
    paths: Vec<PathBuf>,
    launchers: bool,
    recent: Option<bool>,
    permanent: Option<bool>,
    state: State<'_, Arc<Core>>,
) -> Result<Confirmation, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        if paths.is_empty() || paths.len() > 100 {
            return Err("Select between 1 and 100 entries.".into());
        }
        let recent = recent.unwrap_or(false);
        let permanent = permanent.unwrap_or(false);
        if (recent && launchers) || (permanent && !recent) {
            return Err("Permanent deletion is available for post-uninstall leftovers.".into());
        }
        let session = core.session()?;
        let available = if launchers {
            &session.launchers
        } else if recent {
            &session.recent_leftovers
        } else {
            &session.leftovers
        };
        let mut selections = Vec::new();
        for path in &paths {
            let candidate = available
                .iter()
                .find(|candidate| &candidate.path == path && candidate.selectable)
                .ok_or("At least one selected entry is no longer eligible. Scan again.")?;
            if selections.iter().any(|item: &Candidate| {
                item.path == *path || item.path.starts_with(path) || path.starts_with(&item.path)
            }) {
                return Err("Select distinct entries without overlapping folders.".into());
            }
            cleanup::revalidate(&core.locations, candidate)?;
            selections.push(candidate.clone());
        }
        drop(session);
        let count = selections.iter().filter(|item| !item.confirmed).count();
        let mut explanation = if permanent {
            "This permanently deletes the selected entries and their contents. You cannot undo this action."
        } else {
            "Selected entries will move to Recovery. Review their paths before continuing."
        }.to_owned();
        if count > 0 {
            explanation.push_str(&format!(" {count} may still be in use."));
        }
        core.store_preview(
            Preview {
                items: paths.iter().map(|p| p.display().to_string()).collect(),
                explanation,
                authorization: None,
            },
            Action::Cleanup { selections, recent, permanent },
        )
    })
    .await
}

fn remember_uninstall(
    core: &Core,
    app: &Application,
    warnings: &mut Vec<String>,
) -> BTreeSet<String> {
    let inventory = core.inventory(false);
    let mut ids = vec![app.id.clone()];
    if let Ok(inventory) = inventory {
        ids.extend(
            inventory
                .applications
                .iter()
                .filter(|other| other.install == app.install)
                .map(|other| other.id.clone()),
        );
    }
    let recent = ids
        .into_iter()
        .filter(|id| !matches!(app.install, Install::Managed { .. }) && inventory::identifier(id))
        .collect();
    if let Err(error) = History::record(&core.history_path(), &recent, app) {
        warnings.push(format!("Could not record application removal: {error}"));
    }
    recent
}

fn apply_cleanup(
    core: &Core,
    selections: Vec<Candidate>,
    recent: bool,
    permanent: bool,
) -> Result<ResultReport, String> {
    let inventory = core.inventory(true)?;
    let mut warnings = inventory.warnings.clone();
    let launchers = selections.iter().any(|item| item.kind == Kind::Launcher);
    let current = if launchers {
        cleanup::scan_launchers(&core.locations, &inventory, &core.canceled)?
    } else {
        scan_leftovers(core, &inventory, recent, &mut warnings)?
    };
    let mut completed = 0;
    for item in selections {
        if !current.iter().any(|snapshot| {
            snapshot.path == item.path
                && snapshot.kind == item.kind
                && snapshot.selectable
                && snapshot.identity == item.identity
                && (!permanent || snapshot.bytes == item.bytes)
        }) {
            warnings.push(format!(
                "{} changed or is no longer a cleanup candidate.",
                item.path.display()
            ));
            continue;
        }
        let outcome = cleanup::move_to_recovery(&core.locations, &item).and_then(|record| {
            if permanent {
                cleanup::delete_permanently(&core.locations, &record)
            } else {
                Ok(())
            }
        });
        match outcome {
            Ok(()) => completed += 1,
            Err(error) => warnings.push(error),
        }
    }
    if launchers
        && completed > 0
        && let Err(error) = cleanup::refresh_launchers(&core.locations)
    {
        warnings.push(error);
    }
    core.invalidate()?;
    Ok(ResultReport {
        completed,
        warnings,
    })
}

fn move_application_files(
    core: &Core,
    items: Vec<Candidate>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    for item in &items {
        cleanup::revalidate(&core.locations, item)?;
    }
    let mut moved = Vec::<Recovery>::new();
    for item in items {
        match cleanup::move_to_recovery(&core.locations, &item) {
            Ok(record) => moved.push(record),
            Err(mut error) => {
                for record in moved.iter().rev() {
                    if let Err(restore_error) = cleanup::restore(&core.locations, &record.id) {
                        error.push_str(&format!(" An item remains in Recovery: {restore_error}"));
                    }
                }
                return Err(error);
            }
        }
    }
    if moved.iter().any(|record| record.kind == Kind::Launcher)
        && let Err(error) = cleanup::refresh_launchers(&core.locations)
    {
        warnings.push(error);
    }
    Ok(())
}

#[tauri::command]
async fn confirm(
    app: AppHandle,
    token: u64,
    state: State<'_, Arc<Core>>,
) -> Result<ResultReport, String> {
    let core = Arc::clone(&state);
    blocking(move || {
        let _work = core.begin(false)?;
        let (expected, started, action) = core
            .session()?
            .pending
            .take()
            .ok_or("Review an operation first.")?;
        if token != expected || started.elapsed() > Duration::from_secs(300) {
            return Err("The preview expired or changed. Review the operation again.".into());
        }
        match action {
            Action::Cleanup { selections, recent, permanent } => {
                apply_cleanup(&core, selections, recent, permanent)
            }
            Action::Update(version) => {
                updates::apply(&version, |line| { let _ = app.emit("operation-progress", line); })?;
                Ok(ResultReport { completed: 1, warnings: Vec::new() })
            }
            Action::RemoveLauncher(candidate) => {
                let mut warnings = Vec::new();
                move_application_files(&core, vec![*candidate], &mut warnings)?;
                core.invalidate()?;
                Ok(ResultReport { completed: 1, warnings })
            }
            Action::Delete(record) => {
                cleanup::delete_permanently(&core.locations, &record)?;
                Ok(ResultReport {
                    completed: 1,
                    warnings: Vec::new(),
                })
            }
            Action::Uninstall {
                app: target,
                items,
                files,
            } => {
                let progress = |line: String| {
                    let _ = app.emit("operation-progress", line);
                };
                let mut warnings = Vec::new();
                let result = match &target.install {
                    Install::Rpm(spec) => packages::apply_rpm(spec, &items, progress),
                    Install::Flatpak { reference, scope } => {
                        packages::apply_flatpak(reference, scope, progress)
                    }
                    Install::Managed { manager, .. } => manager.apply(&target, progress),
                    Install::AppImage(_) => move_application_files(&core, files, &mut warnings),
                    Install::Toolbox(path) => cleanup::check_app_closed(path, &target.name)
                        .and_then(|()| move_application_files(&core, files, &mut warnings)),
                    Install::Standalone(path) => (|| {
                        let desktop = target.launcher.as_deref().ok_or("The standalone app launcher is unavailable.")?;
                        let inventory = core.inventory(true)?;
                        if !inventory.rpm_complete {
                            return Err("Could not verify the installed applications. Refresh and try again.".into());
                        }
                        cleanup::standalone_candidates(&core.locations, path, desktop, &inventory)?;
                        cleanup::check_app_closed(path, &target.name)?;
                        move_application_files(&core, files, &mut warnings)
                    })(),
                    Install::Manual => Err("Manual removal is unavailable.".into()),
                };
                let recent = result.is_ok().then(|| remember_uninstall(&core, &target, &mut warnings));
                core.invalidate()?;
                result?;
                core.session()?.recent = recent;
                Ok(ResultReport {
                    completed: 1,
                    warnings,
                })
            }
        }
    })
    .await
}

pub fn run() -> Result<(), String> {
    let locations = Locations::current()?;
    let icon_root = locations.data.join("flatpak/exports/share/icons");
    tauri::Builder::default()
        .manage(Arc::new(Core::new(locations)))
        .setup(move |app| {
            if let Some(theme) = gtk::IconTheme::default() {
                theme.append_search_path(&icon_root);
                theme.append_search_path("/var/lib/flatpak/exports/share/icons");
            }
            if let Some(settings) = theme_settings() {
                let handle = app.handle().clone();
                settings.connect_changed(Some("color-scheme"), move |settings, _| {
                    let _ = handle.emit("system-theme", system_dark(settings));
                });
                system_dark(&settings);
                GNOME_SETTINGS.with(|slot| slot.replace(Some(settings)));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let core = window.state::<Arc<Core>>();
                if core.working.load(Ordering::Acquire) && !core.scanning.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.emit(
                        "operation-progress",
                        "Wait for the current operation to finish before closing.",
                    );
                } else {
                    core.canceled.store(true, Ordering::Release);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            configuration,
            show_window,
            save_theme,
            installed,
            cancel_scan,
            app_icons,
            scan_cleanup,
            leftover_contents,
            open_leftover,
            recovery,
            uninstall_history,
            restore,
            preview_uninstall,
            check_updates,
            preview_update,
            preview_remove_launcher,
            preview_cleanup,
            preview_delete,
            confirm
        ])
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string())
}

thread_local! {
    static GNOME_SETTINGS: std::cell::RefCell<Option<gio::Settings>> = const { std::cell::RefCell::new(None) };
}
