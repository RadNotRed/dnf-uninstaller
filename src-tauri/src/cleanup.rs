use crate::inventory::{self, Install, Inventory, Launcher, Locations};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags, ResolveFlags, Stat};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const NOFOLLOW: AtFlags = AtFlags::SYMLINK_NOFOLLOW;
const PRIVATE: Mode = Mode::from_bits_truncate(0o700);
static RECORD_NUMBER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Launcher,
    Icon,
    FlatpakData,
    Config,
    Cache,
    Data,
    State,
    AppImage,
    ToolboxBin,
    StandaloneBinary,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: Kind,
    pub reason: String,
    pub confirmed: bool,
    pub selectable: bool,
    pub bytes: Option<u64>,
    pub file_type: &'static str,
    #[serde(skip)]
    pub identity: Option<Identity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    device: u64,
    inode: u64,
    changed: i64,
    nanos: u64,
}

impl From<Stat> for Identity {
    fn from(s: Stat) -> Self {
        Self {
            device: s.st_dev,
            inode: s.st_ino,
            changed: s.st_ctime,
            nanos: s.st_ctime_nsec,
        }
    }
}

impl Identity {
    fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Recovery {
    pub id: String,
    pub path: PathBuf,
    pub kind: Kind,
}

#[derive(Default, Deserialize, Serialize)]
pub struct History {
    pub removed_ids: BTreeSet<String>,
    #[serde(default)]
    uninstalls: Vec<UninstallEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    earlier_ids: Option<BTreeSet<String>>,
}

#[derive(Deserialize, Serialize)]
struct UninstallEntry {
    name: String,
    app_id: String,
    source: String,
    version: String,
    removed_at: Option<u64>,
}

#[derive(Serialize)]
pub struct UninstallHistory {
    entries: Vec<UninstallEntry>,
    earlier_ids: BTreeSet<String>,
}

const SETTINGS_LIMIT: u64 = 1024 * 1024;
const HISTORY_LIMIT: u64 = 16 * SETTINGS_LIMIT;

impl History {
    pub fn read(path: &Path) -> Result<Self, String> {
        read_json_bounded(path, HISTORY_LIMIT)
    }

    pub fn recent_first(self) -> UninstallHistory {
        let Self {
            removed_ids,
            mut uninstalls,
            earlier_ids,
        } = self;
        uninstalls.reverse();
        UninstallHistory {
            entries: uninstalls,
            earlier_ids: earlier_ids.unwrap_or(removed_ids),
        }
    }

    pub fn record(
        path: &Path,
        ids: &BTreeSet<String>,
        app: &inventory::Application,
    ) -> Result<(), String> {
        let folder = path.parent().ok_or("Missing history directory.")?;
        let dir = open_directory(folder, true)?;
        rustix::fs::flock(&dir, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|error| io_error(path, error))?;
        let mut history = Self::read(path)?;
        history
            .earlier_ids
            .get_or_insert_with(|| history.removed_ids.clone());
        history.removed_ids.extend(ids.iter().cloned());
        history.uninstalls.push(UninstallEntry {
            name: app.name.clone(),
            app_id: app.id.clone(),
            source: app.source.into(),
            version: app.version.clone(),
            removed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|time| time.as_secs()),
        });
        write_json_bounded(path, &history, HISTORY_LIMIT)
    }
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

pub fn secure_dir(path: &Path) -> Result<OwnedFd, String> {
    open_directory(path, false)
}

fn open_directory(path: &Path, create: bool) -> Result<OwnedFd, String> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(io_error(path, "expected an absolute UTF-8 path"));
    }
    let mut fd = rustix::fs::open("/", DIRECTORY, Mode::empty()).map_err(|e| io_error(path, e))?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let next = match rustix::fs::openat(&fd, name, DIRECTORY, Mode::empty()) {
                    Err(rustix::io::Errno::NOENT) if create => {
                        match rustix::fs::mkdirat(&fd, name, PRIVATE) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => return Err(io_error(path, error)),
                        }
                        rustix::fs::openat(&fd, name, DIRECTORY, Mode::empty())
                    }
                    next => next,
                };
                fd = next.map_err(|error| io_error(path, error))?;
            }
            _ => return Err(io_error(path, "unsafe path component")),
        }
    }
    Ok(fd)
}

fn valid_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.is_empty() && !name.chars().any(char::is_control))
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn in_root(path: &Path, root: &Path, direct: bool) -> bool {
    root != Path::new("/")
        && path.strip_prefix(root).is_ok_and(|remainder| {
            let mut parts = remainder.components();
            matches!(parts.next(), Some(Component::Normal(_)))
                && (!direct || parts.next().is_none())
        })
}

fn allowed(locations: &Locations, path: &Path, kind: Kind) -> bool {
    if !valid_name(path) || !in_root(path, &locations.home, false) {
        return false;
    }
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match kind {
        Kind::Launcher => {
            (in_root(path, &locations.data.join("applications"), false)
                && path.extension().is_some_and(|ext| ext == "desktop"))
                || path == locations.config.join("autostart/jetbrains-toolbox.desktop")
        }
        Kind::Icon => {
            in_root(path, &locations.data.join("icons"), false)
                && path
                    .extension()
                    .is_some_and(|ext| matches!(ext.to_str(), Some("png" | "svg" | "xpm")))
        }
        Kind::FlatpakData => {
            in_root(path, &locations.home.join(".var/app"), true) && inventory::identifier(id)
        }
        Kind::Config => in_root(path, &locations.config, true) && inventory::identifier(id),
        Kind::Cache => in_root(path, &locations.cache, true) && inventory::identifier(id),
        Kind::Data => in_root(path, &locations.data, true) && inventory::identifier(id),
        Kind::State => in_root(path, &locations.state, true) && inventory::identifier(id),
        Kind::ToolboxBin => toolbox_bin_path(locations, path),
        Kind::StandaloneBinary => standalone_binary_path(locations, path),
        Kind::AppImage => {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("appimage"))
                && locations
                    .appimage_dirs()
                    .iter()
                    .any(|dir| in_root(path, dir, true))
        }
    }
}

fn object_stat(path: &Path) -> Result<Stat, String> {
    let parent = secure_dir(path.parent().ok_or("Missing parent directory")?)?;
    let name = path.file_name().ok_or("Missing filename")?;
    rustix::fs::statat(&parent, name, NOFOLLOW).map_err(|e| io_error(path, e))
}

fn measure(
    dir: &OwnedFd,
    device: u64,
    budget: &mut usize,
    depth: usize,
    cancel: &AtomicBool,
) -> Option<u64> {
    if depth > 32 {
        return None;
    }
    let iter = rustix::fs::Dir::read_from(dir).ok()?;
    let mut size = 0u64;
    for entry in iter {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        inventory::check_cancel(cancel).ok()?;
        *budget = budget.checked_sub(1)?;
        let stat = rustix::fs::statat(dir, name, NOFOLLOW).ok()?;
        if stat.st_dev != device {
            return None;
        }
        if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
            let subdir = rustix::fs::openat(dir, name, DIRECTORY, Mode::empty()).ok()?;
            size = size.checked_add(measure(&subdir, device, budget, depth + 1, cancel)?)?;
        } else {
            size = size.checked_add(stat.st_size.try_into().ok()?)?;
        }
    }
    Some(size)
}

fn describe(
    locations: &Locations,
    path: PathBuf,
    kind: Kind,
    reason: String,
    confirmed: bool,
    selectable: bool,
    cancel: &AtomicBool,
) -> Candidate {
    let stat = object_stat(&path).ok();
    let identity = stat.map(Identity::from);
    let mut safe_type = false;
    let (file_type, bytes) = match stat.map(|s| (FileType::from_raw_mode(s.st_mode), s)) {
        Some((FileType::RegularFile, s)) => {
            safe_type = kind != Kind::ToolboxBin;
            ("File", s.st_size.try_into().ok())
        }
        Some((FileType::Directory, s)) => {
            safe_type = !matches!(
                kind,
                Kind::Launcher | Kind::Icon | Kind::AppImage | Kind::StandaloneBinary
            );
            let size = secure_dir(&path)
                .ok()
                .and_then(|dir| measure(&dir, s.st_dev, &mut 30_000, 0, cancel));
            ("Folder", size)
        }
        Some((FileType::Symlink, s)) => ("Symbolic link", s.st_size.try_into().ok()),
        _ => ("Unknown", None),
    };
    let owned = stat.is_some_and(|stat| stat.st_uid == rustix::process::geteuid().as_raw());
    let selectable = selectable && safe_type && owned && allowed(locations, &path, kind);
    Candidate {
        path,
        kind,
        reason,
        confirmed,
        selectable,
        bytes,
        file_type,
        identity,
    }
}

fn definitely_missing(program: &Path, locations: &Locations) -> bool {
    let Some(parent) = program.parent() else {
        return false;
    };
    let managed = [Path::new("/usr/bin"), Path::new("/usr/local/bin")]
        .iter()
        .any(|dir| program.parent() == Some(*dir))
        || locations
            .appimage_dirs()
            .iter()
            .any(|dir| dir == parent && dir.starts_with(&locations.home));
    managed
        && valid_name(program)
        && secure_dir(parent).is_ok()
        && fs::symlink_metadata(program).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
}

fn launcher_issue(
    locations: &Locations,
    inventory: &Inventory,
    entry: &Launcher,
) -> Option<(String, bool)> {
    if entry.hidden
        || entry
            .path
            .starts_with(Path::new(crate::managed::SNAP_EXPORT).join("applications"))
    {
        return None;
    }
    if !entry.flatpak.is_empty() && entry.conditional && entry.try_exec.is_empty() {
        // DBus activation is common for Flatpaks; conditionally visible entries are still excluded below.
        return None;
    }
    let path = entry.path.display();
    if entry.conditional {
        // TryExec is a desktop condition. Report the evidence without offering to move it.
        let executable = inventory::executable(&entry.try_exec)?;
        return inventory::resolve(&executable).is_none().then(|| {
            (
                format!("Conditional launcher. TryExec target is unavailable: {executable}."),
                false,
            )
        });
    }
    if !entry.flatpak.is_empty() {
        if inventory.flatpak_complete
            && inventory::identifier(&entry.flatpak)
            && !inventory
                .applications
                .iter()
                .any(|app| app.id == entry.flatpak)
        {
            return Some((
                format!("No installed Flatpak matches {}.", entry.flatpak),
                true,
            ));
        }
        return None;
    }
    if entry.exec.is_empty() {
        return Some((format!("No launch command in {path}."), false));
    }
    let executable = inventory::executable(&entry.exec)?;
    if inventory::resolve(&executable).is_some() {
        return None;
    }
    let confirmed = entry.rpm.is_none()
        && inventory.rpm_complete
        && definitely_missing(Path::new(&executable), locations);
    let reason = if confirmed {
        format!("The executable is missing from a standard installation directory: {executable}.")
    } else if entry.rpm.is_some() {
        format!("The executable is unavailable: {executable}. This launcher is tracked by RPM.")
    } else {
        format!(
            "The executable is unavailable: {executable}. It may be on a removable drive or outside this session’s PATH."
        )
    };
    Some((reason, confirmed))
}

pub fn scan_launchers(
    locations: &Locations,
    inventory: &Inventory,
    cancel: &AtomicBool,
) -> Result<Vec<Candidate>, String> {
    let mut rows = Vec::new();
    for entry in &inventory.launchers {
        inventory::check_cancel(cancel)?;
        if let Some((mut reason, confirmed)) = launcher_issue(locations, inventory, entry) {
            if confirmed && !allowed(locations, &entry.path, Kind::Launcher) {
                reason.push_str(" This launcher is outside your user applications directory.");
            }
            rows.push(describe(
                locations,
                entry.path.clone(),
                Kind::Launcher,
                reason,
                confirmed,
                confirmed,
                cancel,
            ));
        }
    }
    for (path, target) in &inventory.broken_links {
        inventory::check_cancel(cancel)?;
        let reason = format!(
            "This desktop shortcut is a symbolic link with an unavailable target: {}.",
            target.display()
        );
        rows.push(describe(
            locations,
            path.clone(),
            Kind::Launcher,
            reason,
            false,
            false,
            cancel,
        ));
    }
    Ok(rows)
}

pub fn icon_diagnostic(
    locations: &Locations,
    path: PathBuf,
    reason: String,
    cancel: &AtomicBool,
) -> Candidate {
    describe(
        locations,
        path,
        Kind::Launcher,
        reason,
        false,
        false,
        cancel,
    )
}

fn icon_candidates(locations: &Locations, entry: &Launcher) -> Vec<PathBuf> {
    let path = Path::new(&entry.icon);
    if path.is_absolute() {
        return vec![path.to_owned()];
    }
    if entry.icon != entry.id || !inventory::identifier(&entry.id) {
        return Vec::new();
    }
    let root = locations.data.join("icons");
    let mut matches = Vec::new();
    for ext in ["png", "svg", "xpm"] {
        matches.push(root.join(format!("{}.{ext}", entry.icon)));
        for size in [
            "16x16", "22x22", "24x24", "32x32", "48x48", "64x64", "96x96", "128x128", "256x256",
            "512x512", "scalable", "symbolic",
        ] {
            matches.push(
                root.join("hicolor")
                    .join(size)
                    .join("apps")
                    .join(format!("{}.{ext}", entry.icon)),
            );
        }
    }
    matches
}

pub fn scan_leftovers(
    locations: &Locations,
    inventory: &Inventory,
    removed_ids: &BTreeSet<String>,
    recent_only: bool,
    cancel: &AtomicBool,
) -> Result<(Vec<Candidate>, Vec<String>), String> {
    let mut rows = BTreeMap::new();
    let mut warnings = Vec::new();
    let installed: BTreeSet<&str> = inventory
        .applications
        .iter()
        .map(|app| app.id.as_str())
        .collect();

    if inventory.flatpak_complete {
        let root = locations.home.join(".var/app");
        match fs::read_dir(&root) {
            Ok(entries) => {
                for entry in entries {
                    inventory::check_cancel(cancel)?;
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            warnings.push(io_error(&root, error));
                            continue;
                        }
                    };
                    let file_name = entry.file_name();
                    let Some(id) = file_name.to_str() else {
                        continue;
                    };
                    if !inventory::identifier(id)
                        || installed.contains(id)
                        || (recent_only && !removed_ids.contains(id))
                    {
                        continue;
                    }
                    let path = entry.path();
                    if path.is_dir()
                        && (removed_ids.contains(id)
                            || ["data", "cache", "config", ".local"].iter().any(|name| {
                                fs::symlink_metadata(path.join(name)).is_ok_and(|m| m.is_dir())
                            }))
                    {
                        let reason = format!(
                            "Flatpak data directory for {id}; no matching app is installed."
                        );
                        rows.insert(
                            path.clone(),
                            describe(
                                locations,
                                path,
                                Kind::FlatpakData,
                                reason,
                                true,
                                true,
                                cancel,
                            ),
                        );
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => warnings.push(io_error(&root, e)),
        }
    }

    if inventory.rpm_complete && inventory.flatpak_complete && inventory.warnings.is_empty() {
        for id in removed_ids {
            inventory::check_cancel(cancel)?;
            if !inventory::identifier(id) || installed.contains(id.as_str()) {
                continue;
            }
            for (root, kind) in [
                (&locations.config, Kind::Config),
                (&locations.cache, Kind::Cache),
                (&locations.data, Kind::Data),
                (&locations.state, Kind::State),
            ] {
                let path = root.join(id);
                if object_stat(&path).is_ok() {
                    let reason = format!(
                        "Exact desktop ID of {id}, previously uninstalled with this app. This directory may still contain shared data."
                    );
                    rows.insert(
                        path.clone(),
                        describe(locations, path, kind, reason, false, true, cancel),
                    );
                }
            }
        }
    }

    if !recent_only && inventory.warnings.is_empty() {
        for entry in &inventory.launchers {
            inventory::check_cancel(cancel)?;
            if entry.icon.is_empty()
                || !launcher_issue(locations, inventory, entry)
                    .is_some_and(|(_, confirmed)| confirmed)
            {
                continue;
            }
            if !allowed(locations, &entry.path, Kind::Launcher)
                || inventory.launchers.iter().any(|other| {
                    other.path != entry.path
                        && (other.icon == entry.icon
                            || Path::new(&entry.icon).file_stem().and_then(|s| s.to_str())
                                == Some(other.icon.as_str()))
                })
            {
                continue;
            }
            for path in icon_candidates(locations, entry) {
                if allowed(locations, &path, Kind::Icon) && object_stat(&path).is_ok() {
                    let reason = format!(
                        "Referenced by the stale launcher {}. Other programs may still use this icon.",
                        entry.path.display()
                    );
                    rows.insert(
                        path.clone(),
                        describe(locations, path, Kind::Icon, reason, false, true, cancel),
                    );
                }
            }
        }
    }
    inventory::check_cancel(cancel)?;
    Ok((rows.into_values().collect(), warnings))
}

pub fn appimage_candidates(
    locations: &Locations,
    app: &inventory::Application,
    inventory: &Inventory,
) -> Result<Vec<Candidate>, String> {
    let Install::AppImage(path) = &app.install else {
        return Err("Select an AppImage.".into());
    };
    if !inventory::image_integration_path(locations, path) || !inventory::is_appimage(path) {
        return Err(
            "The AppImage changed or is outside the supported installation directories.".into(),
        );
    }
    let candidate = describe(
        locations,
        path.clone(),
        Kind::AppImage,
        "AppImage executable".into(),
        true,
        true,
        &AtomicBool::new(false),
    );
    if !candidate.selectable {
        return Err("The AppImage is outside a safe cleanup directory.".into());
    }
    let mut items = vec![candidate];
    if inventory.rpm_complete {
        for entry in &inventory.launchers {
            if entry.hidden
                || !entry.flatpak.is_empty()
                || entry.rpm.is_some()
                || !allowed(locations, &entry.path, Kind::Launcher)
                || inventory::executable(&entry.exec).is_none_or(|exec| Path::new(&exec) != path)
            {
                continue;
            }
            let launcher = describe(
                locations,
                entry.path.clone(),
                Kind::Launcher,
                "User launcher points directly to the selected AppImage.".into(),
                true,
                true,
                &AtomicBool::new(false),
            );
            if launcher.selectable && !items.iter().any(|item| item.path == launcher.path) {
                items.push(launcher);
            }
        }
    }
    Ok(items)
}

pub fn user_launcher_removable(locations: &Locations, path: &Path) -> bool {
    in_root(path, &locations.data.join("applications"), false)
        && allowed(locations, path, Kind::Launcher)
        && object_stat(path).is_ok_and(|stat| {
            FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_uid == rustix::process::geteuid().as_raw()
        })
}

pub fn manual_launcher_candidate(locations: &Locations, path: &Path) -> Result<Candidate, String> {
    if !user_launcher_removable(locations, path) {
        return Err(
            "This launcher is outside the supported user applications directory or has changed."
                .into(),
        );
    }
    let candidate = describe(
        locations,
        path.to_owned(),
        Kind::Launcher,
        "User application launcher".into(),
        true,
        true,
        &AtomicBool::new(false),
    );
    revalidate(locations, &candidate)?;
    Ok(candidate)
}

fn standalone_binary_path(locations: &Locations, path: &Path) -> bool {
    let Some(folder) = path.parent() else {
        return false;
    };
    let Some(name) = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| inventory::safe_segment(name))
    else {
        return false;
    };
    if folder == locations.home.join(".local/bin") {
        return true;
    }
    let Some(bundle) = folder.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    (bundle == name
        || bundle
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(inventory::safe_segment))
        && ["Downloads", "Applications", ".local/opt"]
            .iter()
            .any(|root| folder.parent() == Some(&locations.home.join(root)))
}

pub fn verified_standalone(locations: &Locations, entry: &Launcher, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if entry.hidden
        || !entry.flatpak.is_empty()
        || entry.rpm.is_some()
        || entry.path
            != locations
                .data
                .join("applications")
                .join(format!("{name}.desktop"))
        || !user_launcher_removable(locations, &entry.path)
        || !allowed(locations, path, Kind::StandaloneBinary)
        || inventory::executable(&entry.exec).is_none_or(|exec| Path::new(&exec) != path)
        || !entry.try_exec.is_empty() && Path::new(&entry.try_exec) != path
    {
        return false;
    }
    let uid = rustix::process::geteuid().as_raw();
    let Some(directory) = path.parent() else {
        return false;
    };
    let Ok(folder) = secure_dir(directory) else {
        return false;
    };
    if directory != locations.home.join(".local/bin") {
        let Ok(entries) = rustix::fs::Dir::read_from(&folder) else {
            return false;
        };
        if !entries.into_iter().all(|entry| {
            entry.is_ok_and(|entry| {
                let file = entry.file_name().to_bytes();
                matches!(file, b"." | b"..") || file == name.as_bytes()
            })
        }) {
            return false;
        }
    }
    let Ok(fd) = rustix::fs::openat(
        &folder,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    if !rustix::fs::fstat(&fd).is_ok_and(|stat| {
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_uid == uid
            && stat.st_mode & 0o111 != 0
    }) {
        return false;
    }
    let mut magic = [0; 4];
    File::from(fd).read_exact(&mut magic).is_ok() && &magic == b"\x7fELF"
}

pub fn standalone_candidates(
    locations: &Locations,
    path: &Path,
    desktop: &Path,
    inventory: &Inventory,
) -> Result<Vec<Candidate>, String> {
    let cancel = AtomicBool::new(false);
    let launcher = describe(
        locations,
        desktop.to_owned(),
        Kind::Launcher,
        "User launcher for the standalone executable".into(),
        true,
        true,
        &cancel,
    );
    let binary = describe(
        locations,
        path.to_owned(),
        Kind::StandaloneBinary,
        "Standalone executable referenced by the user launcher".into(),
        true,
        true,
        &cancel,
    );
    revalidate(locations, &launcher)?;
    revalidate(locations, &binary)?;
    let shared = inventory
        .launchers
        .iter()
        .filter(|entry| entry.path != desktop)
        .any(|entry| {
            inventory::executable(&entry.exec)
                .and_then(|exec| inventory::resolve(&exec))
                .and_then(|resolved| fs::canonicalize(resolved).ok())
                .is_some_and(|resolved| resolved == path)
        });
    const MAX_DESKTOP_BYTES: u64 = 512 * 1024;
    let parent = secure_dir(desktop.parent().ok_or("Missing launcher directory.")?)?;
    let fd = rustix::fs::openat(
        &parent,
        desktop.file_name().ok_or("Missing launcher filename.")?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| io_error(desktop, error))?;
    let stat = rustix::fs::fstat(&fd).map_err(|error| io_error(desktop, error))?;
    if Some(Identity::from(stat)) != launcher.identity
        || launcher.bytes.is_none_or(|bytes| bytes > MAX_DESKTOP_BYTES)
    {
        return Err("The standalone launcher changed or is too large to verify.".into());
    }
    let mut contents = String::new();
    File::from(fd)
        .take(MAX_DESKTOP_BYTES + 1)
        .read_to_string(&mut contents)
        .map_err(|error| io_error(desktop, error))?;
    revalidate(locations, &launcher)?;
    let entry = inventory::launcher(desktop.to_owned(), String::new(), &contents, "");
    if shared || !entry.is_some_and(|entry| verified_standalone(locations, &entry, path)) {
        return Err(
            "The standalone installation changed or its executable is shared by another launcher."
                .into(),
        );
    }
    Ok(vec![binary, launcher])
}

fn toolbox_bin_path(locations: &Locations, path: &Path) -> bool {
    let Some(root) = path.parent().filter(|_| path.ends_with("bin")) else {
        return false;
    };
    if root == locations.data.join("JetBrains/Toolbox") {
        return true;
    }
    let Some(version) = root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("jetbrains-toolbox-"))
    else {
        return false;
    };
    version.split('.').count() >= 3
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && [
            locations.home.clone(),
            locations.home.join("Downloads"),
            locations.home.join("Applications"),
            locations.home.join(".local/opt"),
        ]
        .iter()
        .any(|parent| root.parent() == Some(parent))
}

pub fn verified_toolbox_bin(locations: &Locations, path: &Path) -> bool {
    if !allowed(locations, path, Kind::ToolboxBin) {
        return false;
    }
    let Ok(bin) = secure_dir(path) else {
        return false;
    };
    [
        "jetbrains-toolbox",
        "build.txt",
        "toolbox.svg",
        "libjetbrains-toolbox-interop.so",
    ]
    .iter()
    .all(|name| {
        rustix::fs::statat(&bin, *name, NOFOLLOW).is_ok_and(|stat| {
            FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_uid == rustix::process::geteuid().as_raw()
        })
    })
}

pub fn check_app_closed(path: &Path, name: &str) -> Result<(), String> {
    let processes = fs::read_dir("/proc")
        .map_err(|error| format!("Could not check whether {name} is running. {error}"))?;
    if processes.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.parse::<u32>().is_ok())
            && fs::read_link(entry.path().join("exe")).is_ok_and(|exe| exe.starts_with(path))
    }) {
        return Err(format!("Exit {name} before uninstalling it."));
    }
    Ok(())
}

pub fn toolbox_candidates(locations: &Locations, bin: &Path) -> Result<Vec<Candidate>, String> {
    if !verified_toolbox_bin(locations, bin) {
        return Err("The JetBrains Toolbox installation could not be verified.".into());
    }
    let cancel = AtomicBool::new(false);
    let mut items = vec![describe(
        locations,
        bin.to_owned(),
        Kind::ToolboxBin,
        "Verified JetBrains Toolbox program files".into(),
        true,
        true,
        &cancel,
    )];
    for (path, required) in [
        (
            locations
                .data
                .join("applications/jetbrains-toolbox.desktop"),
            true,
        ),
        (
            locations.config.join("autostart/jetbrains-toolbox.desktop"),
            false,
        ),
    ] {
        let candidate = describe(
            locations,
            path.clone(),
            Kind::Launcher,
            "JetBrains Toolbox launcher".into(),
            true,
            true,
            &cancel,
        );
        let matches = candidate.selectable
            && candidate.bytes.is_some_and(|bytes| bytes <= 512 * 1024)
            && fs::read_to_string(&path)
                .ok()
                .and_then(|text| inventory::launcher(path, "jetbrains-toolbox".into(), &text, ""))
                .and_then(|entry| inventory::executable(&entry.exec))
                .is_some_and(|exec| Path::new(&exec) == bin.join("jetbrains-toolbox"));
        if matches {
            items.push(candidate);
        } else if required {
            return Err(
                "The JetBrains Toolbox launcher changed. Refresh and review it again.".into(),
            );
        }
    }
    for item in &items {
        revalidate(locations, item)?;
    }
    Ok(items)
}

fn recovery_path(locations: &Locations) -> PathBuf {
    locations.data.join("application-cleanup-recovery")
}

fn open_recovery(locations: &Locations, create: bool) -> Result<Option<OwnedFd>, String> {
    if !in_root(&locations.data, &locations.home, false) {
        return Err("Recovery requires a user data directory inside your home directory.".into());
    }
    let root = recovery_path(locations);
    if !create && fs::symlink_metadata(&root).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return Ok(None);
    }
    let fd = open_directory(&root, create)?;
    let stat = rustix::fs::fstat(&fd).map_err(|e| io_error(&root, e))?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
        return Err(io_error(
            &root,
            "recovery directory must be private and owned by you",
        ));
    }
    Ok(Some(fd))
}

fn lock_recovery(root: &OwnedFd) -> Result<(), String> {
    rustix::fs::flock(root, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|error| format!("Recovery is busy or could not be locked: {error}"))
}

pub fn revalidate(locations: &Locations, candidate: &Candidate) -> Result<Identity, String> {
    if !candidate.selectable || !allowed(locations, &candidate.path, candidate.kind) {
        return Err("This entry is outside the allowed cleanup locations.".into());
    }
    let stat = object_stat(&candidate.path)?;
    let identity = Identity::from(stat);
    if Some(identity) != candidate.identity || stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(io_error(
            &candidate.path,
            "the entry changed or belongs to another user",
        ));
    }
    Ok(identity)
}

#[derive(Serialize)]
pub struct LeftoverContents {
    entries: Vec<LeftoverEntry>,
    more: bool,
}

#[derive(Serialize)]
struct LeftoverEntry {
    name: String,
    folder: bool,
    browsable: bool,
}

pub fn leftover_contents(
    locations: &Locations,
    candidate: &Candidate,
    folders: &[String],
) -> Result<LeftoverContents, String> {
    if folders.len() > 16 {
        return Err("Open the folder in your file manager to browse further.".into());
    }
    let identity = revalidate(locations, candidate)?;
    let mut dir = secure_dir(&candidate.path)?;
    let stat = rustix::fs::fstat(&dir).map_err(|error| io_error(&candidate.path, error))?;
    if Identity::from(stat) != identity {
        return Err("The folder changed. Scan again.".into());
    }
    for name in folders {
        if !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(component)) if component == name.as_str()
        ) {
            return Err("Invalid folder name.".into());
        }
        dir = rustix::fs::openat2(
            &dir,
            name.as_str(),
            DIRECTORY,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_XDEV | ResolveFlags::NO_SYMLINKS,
        )
        .map_err(|error| format!("Could not open the subfolder: {error}"))?;
    }
    let mut entries = Vec::new();
    let mut more = false;
    let iter =
        rustix::fs::Dir::read_from(&dir).map_err(|error| io_error(&candidate.path, error))?;
    for entry in iter {
        let entry = entry.map_err(|error| io_error(&candidate.path, error))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if entries.len() == 40 {
            more = true;
            break;
        }
        let stat = rustix::fs::statat(&dir, name, NOFOLLOW)
            .map_err(|error| io_error(&candidate.path, error))?;
        let folder = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
        entries.push(LeftoverEntry {
            name: name.to_string_lossy().into_owned(),
            folder,
            browsable: folder && name.to_str().is_ok() && folders.len() < 16,
        });
    }
    entries.sort_unstable_by(|first, second| first.name.cmp(&second.name));
    Ok(LeftoverContents { entries, more })
}

pub fn move_to_recovery(locations: &Locations, candidate: &Candidate) -> Result<Recovery, String> {
    let identity = revalidate(locations, candidate)?;
    let root = open_recovery(locations, true)?.ok_or("Could not open recovery directory.")?;
    lock_recovery(&root)?;
    let id = format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
        std::process::id(),
        RECORD_NUMBER.fetch_add(1, Ordering::Relaxed)
    );
    let path = candidate.path.clone();
    let parent = secure_dir(path.parent().ok_or("Missing original parent")?)?;
    let name = path.file_name().ok_or("Missing original filename")?;
    let record = Recovery {
        id,
        path: path.clone(),
        kind: candidate.kind,
    };
    rustix::fs::mkdirat(&root, record.id.as_str(), PRIVATE).map_err(|e| io_error(&path, e))?;
    let directory = rustix::fs::openat(&root, record.id.as_str(), DIRECTORY, Mode::empty())
        .map_err(|e| io_error(&path, e))?;
    let mut moved = false;
    let result = (|| {
        let file = rustix::fs::openat(
            &directory,
            "record.json",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|e| io_error(&path, e))?;
        let mut file = File::from(file);
        serde_json::to_writer(&mut file, &record).map_err(|e| io_error(&path, e))?;
        file.sync_all().map_err(|e| io_error(&path, e))?;
        let stat = rustix::fs::statat(&parent, name, NOFOLLOW).map_err(|e| io_error(&path, e))?;
        if Identity::from(stat) != identity {
            return Err(io_error(&path, "the entry changed during cleanup"));
        }
        rustix::fs::renameat_with(&parent, name, &directory, "item", RenameFlags::NOREPLACE)
            .map_err(|e| {
                io_error(
                    &path,
                    if e == rustix::io::Errno::XDEV {
                        "recovery is on another filesystem; item was left in place".to_owned()
                    } else {
                        e.to_string()
                    },
                )
            })?;
        moved = true;
        let moved_identity = rustix::fs::statat(&directory, "item", NOFOLLOW)
            .map(Identity::from)
            .map_err(|e| io_error(&path, e))?;
        if !identity.same_inode(moved_identity) {
            rustix::fs::renameat_with(&directory, "item", &parent, name, RenameFlags::NOREPLACE)
                .map_err(|e| {
                    io_error(
                        &path,
                        format!("entry changed during cleanup; inspect Recovery ({e})"),
                    )
                })?;
            moved = false;
            return Err(io_error(
                &path,
                "entry changed during cleanup and was put back",
            ));
        }
        Ok(())
    })();
    if !moved {
        // A failed move can leave an empty record. It never contains user data.
        let _ = rustix::fs::unlinkat(&directory, "record.json", AtFlags::empty());
        let _ = rustix::fs::unlinkat(&root, record.id.as_str(), AtFlags::REMOVEDIR);
    }
    result?;
    Ok(record)
}

fn recovery_record(root: &OwnedFd, id: &str) -> Result<(OwnedFd, Recovery), String> {
    if id.is_empty() || id.len() > 80 || !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("Invalid recovery ID.".into());
    }
    let directory =
        rustix::fs::openat(root, id, DIRECTORY, Mode::empty()).map_err(|e| e.to_string())?;
    rustix::fs::statat(&directory, "item", NOFOLLOW).map_err(|e| e.to_string())?;
    let file = rustix::fs::openat(
        &directory,
        "record.json",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let record: Recovery =
        serde_json::from_reader(File::from(file).take(64 * 1024)).map_err(|e| e.to_string())?;
    if record.id != id {
        return Err("Recovery record does not match its directory.".into());
    }
    Ok((directory, record))
}

pub fn list_recovery(locations: &Locations) -> Result<Vec<Recovery>, String> {
    let Some(root) = open_recovery(locations, false)? else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for entry in rustix::fs::Dir::read_from(&root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().to_str().map_err(|e| e.to_string())?;
        if matches!(id, "." | "..") {
            continue;
        }
        let Ok((_, record)) = recovery_record(&root, id) else {
            continue;
        };
        rows.push(record);
    }
    rows.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(rows)
}

pub fn restore(locations: &Locations, id: &str) -> Result<Recovery, String> {
    let root = open_recovery(locations, false)?.ok_or("There are no items in recovery.")?;
    lock_recovery(&root)?;
    let (directory, record) = recovery_record(&root, id)?;
    let path = &record.path;
    if !allowed(locations, path, record.kind) {
        return Err("The original location is no longer allowed.".into());
    }
    let parent = secure_dir(path.parent().ok_or("Missing original parent")?)?;
    let name = path.file_name().ok_or("Missing original filename")?;
    rustix::fs::renameat_with(&directory, "item", &parent, name, RenameFlags::NOREPLACE)
        .map_err(|e| io_error(path, e))?;
    // Once the item is restored, record cleanup must never move it back or overwrite it.
    let _ = rustix::fs::unlinkat(&directory, "record.json", AtFlags::empty());
    let _ = rustix::fs::unlinkat(&root, id, AtFlags::REMOVEDIR);
    Ok(record)
}

pub fn permanent_preview(locations: &Locations, id: &str) -> Result<Recovery, String> {
    let root = open_recovery(locations, false)?.ok_or("There are no items in Recovery.")?;
    recovery_record(&root, id).map(|(_, record)| record)
}

fn erase(parent: &OwnedFd, name: &std::ffi::CStr, depth: usize) -> Result<(), String> {
    if depth > 128 {
        return Err("The folder nesting limit was exceeded.".into());
    }
    let stat = rustix::fs::statat(parent, name, NOFOLLOW).map_err(|error| error.to_string())?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return rustix::fs::unlinkat(parent, name, AtFlags::empty())
            .map_err(|error| error.to_string());
    }
    // NO_XDEV also blocks bind mounts on the same device.
    let directory = rustix::fs::openat2(
        parent,
        name,
        DIRECTORY,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_XDEV | ResolveFlags::NO_SYMLINKS,
    )
    .map_err(|error| error.to_string())?;
    if !Identity::from(stat).same_inode(Identity::from(
        rustix::fs::fstat(&directory).map_err(|error| error.to_string())?,
    )) {
        return Err("A folder changed during deletion.".into());
    }
    for entry in rustix::fs::Dir::read_from(&directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let child = entry.file_name();
        if !matches!(child.to_bytes(), b"." | b"..") {
            erase(&directory, child, depth + 1)?;
        }
    }
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|error| error.to_string())
}

pub fn delete_permanently(locations: &Locations, expected: &Recovery) -> Result<(), String> {
    let root = open_recovery(locations, false)?.ok_or("There are no items in Recovery.")?;
    lock_recovery(&root)?;
    let (directory, record) = recovery_record(&root, &expected.id)?;
    if record.path != expected.path || record.kind != expected.kind {
        return Err("The Recovery entry changed. Review it again.".into());
    }
    erase(&directory, c"item", 0).map_err(|error| {
        format!(
            "Could not finish deleting {}. Any remaining files are still in Recovery. {error}",
            record.path.display()
        )
    })?;
    // Empty metadata directories are harmless if record cleanup fails.
    let _ = rustix::fs::unlinkat(&directory, "record.json", AtFlags::empty());
    let _ = rustix::fs::unlinkat(&root, expected.id.as_str(), AtFlags::REMOVEDIR);
    Ok(())
}

pub fn refresh_launchers(locations: &Locations) -> Result<(), String> {
    let directory = locations.data.join("applications");
    let fd = secure_dir(&directory)?;
    let anchored = format!("/proc/{}/fd/{}", std::process::id(), fd.as_raw_fd());
    let output = std::process::Command::new("/usr/bin/update-desktop-database")
        .arg(anchored)
        .output()
        .map_err(|e| format!("Could not refresh desktop MIME associations: {e}"))?;
    if !output.status.success() {
        return Err(crate::packages::command_error(
            "Desktop MIME refresh",
            &output,
        ));
    }
    Ok(())
}

pub fn read_json<T: Default + for<'a> Deserialize<'a>>(path: &Path) -> Result<T, String> {
    read_json_bounded(path, SETTINGS_LIMIT)
}

fn read_json_bounded<T: Default + for<'a> Deserialize<'a>>(
    path: &Path,
    limit: u64,
) -> Result<T, String> {
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound) {
        return Ok(T::default());
    }
    let parent = secure_dir(path.parent().ok_or("Missing settings directory.")?)?;
    let file = rustix::fs::openat(
        &parent,
        path.file_name().ok_or("Missing settings filename.")?,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| io_error(path, error))?;
    let file = File::from(file);
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= limit)
    {
        return Err(io_error(
            path,
            format!(
                "expected a regular file no larger than {} MiB",
                limit / (1024 * 1024)
            ),
        ));
    }
    serde_json::from_reader(file.take(limit)).map_err(|error| io_error(path, error))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_json_bounded(path, value, SETTINGS_LIMIT)
}

fn write_json_bounded(path: &Path, value: &impl Serialize, limit: u64) -> Result<(), String> {
    let dir = open_directory(path.parent().ok_or("Missing settings directory.")?, true)?;
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    let mut temp = tempfile::NamedTempFile::new_in(&anchored).map_err(|e| io_error(path, e))?;
    serde_json::to_writer(&mut temp, value).map_err(|e| io_error(path, e))?;
    temp.flush().map_err(|e| io_error(path, e))?;
    if temp
        .as_file()
        .metadata()
        .map_err(|e| io_error(path, e))?
        .len()
        > limit
    {
        return Err(io_error(path, "the file reached its size limit"));
    }
    temp.as_file().sync_all().map_err(|e| io_error(path, e))?;
    temp.persist(anchored.join(path.file_name().ok_or("Missing settings filename.")?))
        .map_err(|e| io_error(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn locations(home: &Path) -> Locations {
        Locations {
            home: home.to_owned(),
            data: home.join(".local/share"),
            config: home.join(".config"),
            cache: home.join(".cache"),
            state: home.join(".local/state"),
            desktop: vec![],
        }
    }

    #[test]
    fn recoverable_cleanup_refuses_symlinks_changed_targets_and_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let launchers = layout.data.join("applications");
        fs::create_dir_all(&launchers).unwrap();
        let path = launchers.join("stale.desktop");
        fs::write(&path, "example").unwrap();
        let cancel = AtomicBool::new(false);
        let candidate = describe(
            &layout,
            path.clone(),
            Kind::Launcher,
            String::new(),
            true,
            true,
            &cancel,
        );
        let record = move_to_recovery(&layout, &candidate).unwrap();
        assert!(!path.exists());
        assert_eq!(list_recovery(&layout).unwrap().len(), 1);
        fs::write(&path, "replacement").unwrap();
        assert!(restore(&layout, &record.id).is_err());
        assert!(move_to_recovery(&layout, &candidate).is_err());
        fs::remove_file(&path).unwrap();
        restore(&layout, &record.id).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "example");
        assert!(list_recovery(&layout).unwrap().is_empty());
        fs::rename(&launchers, launchers.with_extension("old")).unwrap();
        symlink(launchers.with_extension("old"), &launchers).unwrap();
        assert!(secure_dir(&launchers).is_err());
        assert!(!allowed(
            &layout,
            &launchers.join("../stale.desktop"),
            Kind::Launcher
        ));
    }

    #[test]
    fn standalone_recipe_requires_an_unshared_single_binary_and_preserves_data() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let folder = layout.home.join("Downloads/horizon-launcher-linux-x86_64");
        let executable = folder.join("horizon-launcher");
        let launcher = layout.data.join("applications/horizon-launcher.desktop");
        let data = layout.data.join("horizon/worlds");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::write(&executable, b"\x7fELFtest").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let contents = format!(
            "[Desktop Entry]\nType=Application\nName=Horizon\nExec=\"{}\"\n",
            executable.display()
        );
        fs::write(&launcher, &contents).unwrap();
        let mut inventory = Inventory::default();
        let alias = launcher.with_file_name("other.desktop");
        inventory
            .launchers
            .push(inventory::launcher(alias, "other".into(), &contents, "").unwrap());
        assert!(standalone_candidates(&layout, &executable, &launcher, &inventory).is_err());
        inventory.launchers.clear();
        fs::write(folder.join("extra"), "keep").unwrap();
        assert!(standalone_candidates(&layout, &executable, &launcher, &inventory).is_err());
        fs::remove_file(folder.join("extra")).unwrap();

        let items = standalone_candidates(&layout, &executable, &launcher, &inventory).unwrap();
        let records: Vec<_> = items
            .iter()
            .map(|item| move_to_recovery(&layout, item).unwrap())
            .collect();
        assert!(!executable.exists() && !launcher.exists() && data.is_dir());
        for record in records {
            restore(&layout, &record.id).unwrap();
        }
        assert!(executable.exists() && launcher.exists());
    }

    #[test]
    fn local_bin_recipe_and_launcher_only_removal_are_separate() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let bin = layout.home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("gpgfrontend");
        let unrelated = bin.join("other-app");
        fs::write(&executable, b"\x7fELFtest").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&unrelated, "keep").unwrap();
        let desktop = layout.data.join("applications/gpgfrontend.desktop");
        fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        fs::write(
            &desktop,
            format!(
                "[Desktop Entry]\nType=Application\nName=GpgFrontend\nExec={}\n",
                executable.display()
            ),
        )
        .unwrap();
        assert_eq!(
            standalone_candidates(&layout, &executable, &desktop, &Inventory::default())
                .unwrap()
                .len(),
            2
        );
        let candidate = manual_launcher_candidate(&layout, &desktop).unwrap();
        let record = move_to_recovery(&layout, &candidate).unwrap();
        assert!(!desktop.exists() && executable.is_file() && unrelated.is_file());
        restore(&layout, &record.id).unwrap();
        let alias = desktop.with_file_name("symlink.desktop");
        symlink(&desktop, &alias).unwrap();
        assert!(manual_launcher_candidate(&layout, &alias).is_err());
    }

    #[test]
    fn toolbox_recipe_preserves_ides_and_unrelated_files() {
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let archive = layout.home.join("Downloads/jetbrains-toolbox-3.4.3.81140");
        let bin = archive.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for file in [
            "jetbrains-toolbox",
            "build.txt",
            "toolbox.svg",
            "libjetbrains-toolbox-interop.so",
        ] {
            fs::write(bin.join(file), "fixture").unwrap();
        }
        let note = archive.join("notes.txt");
        fs::write(&note, "keep").unwrap();
        let installed_ide = layout.data.join("JetBrains/Toolbox/apps/idea");
        fs::create_dir_all(&installed_ide).unwrap();
        let desktop = layout.data.join("applications/jetbrains-toolbox.desktop");
        let autostart = layout.config.join("autostart/jetbrains-toolbox.desktop");
        for launcher in [&desktop, &autostart] {
            fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            fs::write(
                launcher,
                format!(
                    "[Desktop Entry]\nType=Application\nName=JetBrains Toolbox\nExec={} %u\n",
                    bin.join("jetbrains-toolbox").display()
                ),
            )
            .unwrap();
        }

        let items = toolbox_candidates(&layout, &bin).unwrap();
        assert_eq!(items.len(), 3);
        let records: Vec<_> = items
            .iter()
            .map(|item| move_to_recovery(&layout, item).unwrap())
            .collect();
        assert!(!bin.exists() && !desktop.exists() && !autostart.exists());
        assert!(installed_ide.is_dir() && note.is_file());
        for record in records.into_iter().rev() {
            restore(&layout, &record.id).unwrap();
        }
        assert!(bin.is_dir() && desktop.is_file() && autostart.is_file());
        fs::remove_file(bin.join("build.txt")).unwrap();
        assert!(toolbox_candidates(&layout, &bin).is_err());
    }

    #[test]
    fn uninstall_history_retains_earlier_ids_and_records_repeated_removals() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.json");
        fs::write(&path, r#"{"removed_ids":["org.example.Earlier"]}"#).unwrap();
        let before = History::read(&path).unwrap().recent_first();
        assert!(before.entries.is_empty());
        assert!(before.earlier_ids.contains("org.example.Earlier"));
        let mut app = inventory::Application {
            key: "appimage-key".into(),
            name: "Example".into(),
            source: "AppImage",
            version: "1".into(),
            id: String::new(),
            summary: String::new(),
            origin: String::new(),
            launcher: None,
            removable: true,
            launcher_removable: false,
            icon: String::new(),
            install: Install::AppImage(temp.path().join("Example.AppImage")),
        };
        History::record(&path, &BTreeSet::new(), &app).unwrap();
        app.version = "2".into();
        let ids = ["org.example.Earlier".into(), "org.example.New".into()].into();
        History::record(&path, &ids, &app).unwrap();
        let history = History::read(&path).unwrap();
        assert!(history.removed_ids.contains("org.example.New"));
        let newest_first = history.recent_first();
        assert_eq!(newest_first.earlier_ids, before.earlier_ids);
        assert_eq!(newest_first.entries.len(), 2);
        assert_eq!(newest_first.entries[0].version, "2");
        assert_eq!(newest_first.entries[1].version, "1");
        assert!(newest_first.entries[0].removed_at.is_some());
    }

    #[test]
    fn recent_leftovers_only_include_related_paths_and_preview_without_following_links() {
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let id = "org.example.Removed";
        let cache = layout.cache.join(id);
        fs::create_dir_all(cache.join("subfolder")).unwrap();
        let flatpak_data = layout.home.join(".var/app");
        fs::create_dir_all(flatpak_data.join(id).join("data")).unwrap();
        fs::create_dir_all(flatpak_data.join("org.example.Unrelated").join("data")).unwrap();
        symlink(cache.join("subfolder"), cache.join("link")).unwrap();
        let removed_ids = [id.to_owned()].into();
        let inventory = Inventory {
            flatpak_complete: true,
            rpm_complete: true,
            ..Inventory::default()
        };
        let cancel = AtomicBool::new(false);
        let (items, _) = scan_leftovers(&layout, &inventory, &removed_ids, true, &cancel).unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.path == flatpak_data.join(id)));
        let candidate = items.iter().find(|item| item.path == cache).unwrap();
        let contents = leftover_contents(&layout, candidate, &[]).unwrap();
        assert_eq!(contents.entries.len(), 2);
        assert_eq!(contents.entries[0].name, "link");
        assert!(!contents.entries[0].folder);
        assert!(contents.entries[1].folder);
        assert!(leftover_contents(&layout, candidate, &["link".into()]).is_err());
        assert!(leftover_contents(&layout, candidate, &["subfolder/..".into()]).is_err());
        assert!(
            leftover_contents(&layout, candidate, &["subfolder".into()])
                .unwrap()
                .entries
                .is_empty()
        );
        fs::rename(&cache, cache.with_extension("old")).unwrap();
        symlink(cache.with_extension("old"), &cache).unwrap();
        assert!(leftover_contents(&layout, candidate, &[]).is_err());
    }

    #[test]
    fn permanent_recovery_deletion_does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        let outside = layout.home.join("outside");
        fs::write(&outside, "keep").unwrap();
        let path = layout.cache.join("org.example.Stale");
        fs::create_dir_all(&path).unwrap();
        symlink(&outside, path.join("outside-link")).unwrap();
        let candidate = describe(
            &layout,
            path.clone(),
            Kind::Cache,
            String::new(),
            true,
            true,
            &AtomicBool::new(false),
        );
        let record = move_to_recovery(&layout, &candidate).unwrap();
        delete_permanently(&layout, &record).unwrap();
        assert!(!path.exists());
        assert!(list_recovery(&layout).unwrap().is_empty());
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
    }

    #[test]
    fn stale_launcher_classification_retains_conditional_and_external_apps() {
        let temp = tempfile::tempdir().unwrap();
        let layout = locations(temp.path());
        fs::create_dir_all(layout.home.join("AppImages")).unwrap();
        let path = layout.home.join("AppImages/test.AppImage");
        let entry = Launcher {
            path: layout.data.join("applications/stale.desktop"),
            id: "stale".into(),
            name: "Test".into(),
            summary: String::new(),
            exec: path.display().to_string(),
            try_exec: String::new(),
            icon: String::new(),
            hidden: false,
            conditional: false,
            flatpak: String::new(),
            snap: String::new(),
            appimage_version: String::new(),
            rpm: None,
        };
        let inventory = Inventory {
            rpm_complete: true,
            ..Default::default()
        };
        assert!(launcher_issue(&layout, &inventory, &entry).unwrap().1);
        let conditional = Launcher {
            conditional: true,
            try_exec: entry.exec.clone(),
            ..entry.clone()
        };
        assert!(!launcher_issue(&layout, &inventory, &conditional).unwrap().1);
        let outside = Launcher {
            exec: "/mnt/removable/test.AppImage".into(),
            ..entry.clone()
        };
        assert!(!launcher_issue(&layout, &inventory, &outside).unwrap().1);
        let hidden = Launcher {
            hidden: true,
            ..entry
        };
        assert!(launcher_issue(&layout, &inventory, &hidden).is_none());
    }
}
