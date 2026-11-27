use crate::managed::{self, Manager};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct Locations {
    pub home: PathBuf,
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
    pub desktop: Vec<PathBuf>,
}

impl Locations {
    pub fn current() -> Result<Self, String> {
        let home = absolute_env("HOME").ok_or("HOME is unavailable")?;
        let data = absolute_env("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let config = absolute_env("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let cache = absolute_env("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
        let state = absolute_env("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state"));
        let mut desktop = vec![data.join("applications")];
        let mut system_data: Vec<_> = env::var_os("XDG_DATA_DIRS")
            .map(|value| {
                env::split_paths(&value)
                    .filter(|path| path.is_absolute() && path.to_str().is_some())
                    .collect()
            })
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
        system_data.extend([
            data.join("flatpak/exports/share"),
            "/var/lib/flatpak/exports/share".into(),
            managed::SNAP_EXPORT.into(),
        ]);
        for dir in system_data {
            let dir = dir.join("applications");
            if !desktop.contains(&dir) {
                desktop.push(dir);
            }
        }
        Ok(Self {
            home,
            data,
            config,
            cache,
            state,
            desktop,
        })
    }

    pub fn appimage_dirs(&self) -> [PathBuf; 4] {
        [
            self.home.join("Applications"),
            self.home.join("AppImages"),
            self.home.join(".local/bin"),
            self.data.join("appimages"),
        ]
    }
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.to_str().is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Rpm(String),
    Flatpak {
        reference: String,
        scope: String,
    },
    Managed {
        manager: Manager,
        package: String,
        revision: String,
    },
    AppImage(PathBuf),
    Toolbox(PathBuf),
    Standalone(PathBuf),
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct Application {
    pub key: String,
    pub name: String,
    pub source: &'static str,
    pub version: String,
    pub id: String,
    pub summary: String,
    pub origin: String,
    pub launcher: Option<PathBuf>,
    pub removable: bool,
    pub launcher_removable: bool,
    #[serde(skip)]
    pub icon: String,
    #[serde(skip)]
    pub install: Install,
}

#[derive(Debug, Clone)]
pub struct Launcher {
    pub path: PathBuf,
    pub id: String,
    pub name: String,
    pub summary: String,
    pub exec: String,
    pub try_exec: String,
    pub icon: String,
    pub hidden: bool,
    pub conditional: bool,
    pub flatpak: String,
    pub snap: String,
    pub appimage_version: String,
    pub rpm: Option<(String, String)>,
}

#[derive(Default)]
pub struct Inventory {
    pub applications: Vec<Application>,
    pub launchers: Vec<Launcher>,
    pub broken_links: Vec<(PathBuf, PathBuf)>,
    pub warnings: Vec<String>,
    pub managed_warnings: Vec<String>,
    pub flatpak_complete: bool,
    pub rpm_complete: bool,
}

pub fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Scan canceled.".into())
    } else {
        Ok(())
    }
}

fn unescape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => output.push(' '),
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

pub(crate) fn launcher(
    path: PathBuf,
    id: String,
    contents: &str,
    locale: &str,
) -> Option<Launcher> {
    let mut in_entry = false;
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            fields.entry(key).or_insert(value);
        }
    }
    let get = |key: &str| unescape(fields.get(key).copied().unwrap_or_default());
    let hidden = get("Hidden") == "true";
    if get("Type") != "Application" && !hidden {
        return None;
    }
    let localized = |key: &str| {
        let language = locale.split(['_', '@']).next().unwrap_or_default();
        [locale, language]
            .into_iter()
            .find_map(|locale| {
                let value = get(&format!("{key}[{locale}]"));
                (!value.is_empty()).then_some(value)
            })
            .unwrap_or_else(|| get(key))
    };
    let name = localized("Name");
    Some(Launcher {
        path,
        name: if name.is_empty() { id.clone() } else { name },
        id,
        summary: localized("Comment"),
        exec: get("Exec"),
        try_exec: get("TryExec"),
        icon: get("Icon"),
        hidden: hidden || get("NoDisplay") == "true",
        conditional: get("DBusActivatable") == "true"
            || !get("OnlyShowIn").is_empty()
            || !get("NotShowIn").is_empty()
            || !get("TryExec").is_empty()
            || !get("AutostartCondition").is_empty()
            || get("X-GNOME-HiddenUnderSystemd") == "true",
        flatpak: get("X-Flatpak"),
        snap: get("X-SnapInstanceName"),
        appimage_version: get("X-AppImage-Version"),
        rpm: None,
    })
}

/// Read only the direct executable or the common "env KEY=value program" form.
pub fn executable(exec: &str) -> Option<String> {
    fn token(input: &str) -> Option<(String, &str)> {
        let input = input.trim_start();
        if let Some(quoted) = input.strip_prefix('"') {
            let end = quoted.find('"')?;
            let (value, tail) = (&quoted[..end], &quoted[end + 1..]);
            if value.contains(['\\', '$', '`', '%'])
                || !tail.chars().next().is_none_or(char::is_whitespace)
            {
                return None;
            }
            return Some((value.to_owned(), tail));
        }
        let end = input.find(char::is_whitespace).unwrap_or(input.len());
        let value = &input[..end];
        if value.is_empty()
            || value.contains(['\\', '"', '\'', '%', '$', '`', ';', '|', '&', '<', '>'])
        {
            return None;
        }
        Some((value.to_owned(), &input[end..]))
    }
    let (mut program, mut tail) = token(exec)?;
    if matches!(program.as_str(), "env" | "/usr/bin/env" | "/bin/env") {
        loop {
            (program, tail) = token(tail)?;
            if let Some((name, _)) = program.split_once('=') {
                if name.is_empty()
                    || name == "PATH"
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return None;
                }
            } else {
                break;
            }
        }
    }
    (!program.starts_with('-') && (!program.contains('/') || program.starts_with('/')))
        .then_some(program)
}

pub fn resolve(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return runnable(path).then(|| path.to_owned());
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|path| runnable(path))
}

fn runnable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

pub fn is_appimage(path: &Path) -> bool {
    use std::io::Read;
    let mut bytes = [0; 11];
    if !runnable(path) {
        return false;
    }
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
        && bytes[..4] == *b"\x7fELF"
        && bytes[8..10] == *b"AI"
        && matches!(bytes[10], 1 | 2)
}

pub fn identifier(text: &str) -> bool {
    text.len() <= 255
        && text.split('.').count() >= 3
        && text.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
}

pub fn safe_segment(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 255
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

pub fn rpm_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

pub fn protected_rpm(name: &str) -> bool {
    matches!(
        name,
        "dnf"
            | "dnf5"
            | "rpm"
            | "glibc"
            | "systemd"
            | "sudo"
            | "polkit"
            | "bash"
            | "flatpak"
            | "gnome-shell"
            | "gnome-session"
            | "gdm"
            | "NetworkManager"
            | "linux-rust-uninstaller"
            | "application-cleanup"
    ) || [
        "kernel",
        "grub2",
        "shim",
        "fedora-release",
        "systemd",
        "gnome-session",
    ]
    .iter()
    .any(|prefix| name.starts_with(&format!("{prefix}-")))
}

fn walk_desktop(
    root: &Path,
    dir: &Path,
    depth: u8,
    locale: &str,
    inventory: &mut Inventory,
    cancel: &AtomicBool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    if depth > 8 {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        check_cancel(cancel)?;
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            walk_desktop(root, &path, depth + 1, locale, inventory, cancel)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "desktop")
        {
            let Some(id) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
                continue;
            };
            let id = id.trim_end_matches(".desktop").replace('/', "-");
            if !(kind.is_file() || kind.is_symlink()) {
                continue;
            }
            let metadata = fs::metadata(&path);
            if kind.is_symlink()
                && metadata
                    .as_ref()
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                && let Ok(target) = fs::read_link(&path)
            {
                inventory.broken_links.push((path, target));
                continue;
            }
            if !metadata.is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 512 * 1024) {
                continue;
            }
            if let Ok(contents) = fs::read_to_string(&path)
                && let Some(entry) = launcher(path, id, &contents, locale)
            {
                inventory.launchers.push(entry);
            }
        }
    }
    Ok(())
}

fn flatpaks() -> Result<(Vec<Application>, Vec<String>), String> {
    let output = Command::new("/usr/bin/flatpak")
        .args([
            "list",
            "--app",
            "--columns=ref:f,name:f,installation:f,version:f",
        ])
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(crate::packages::command_error("Flatpak inventory", &output));
    }
    let mut warnings = Vec::new();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        warnings.push(format!("Flatpak inventory reported: {}", stderr.trim()));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut apps = Vec::new();
    let mut skipped = 0;
    for line in text.lines().filter(|line| !line.is_empty()) {
        match flatpak_record(line) {
            Some(app) => apps.push(app),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        warnings.push(format!(
            "Flatpak inventory skipped {skipped} unrecognized app records."
        ));
    }
    Ok((apps, warnings))
}

fn flatpak_record(line: &str) -> Option<Application> {
    // Flatpak omits trailing columns when the app has no release version.
    let mut columns = line.splitn(4, '\t');
    let (id, arch, branch) = parse_ref(columns.next()?)?;
    let name = columns.next()?;
    let scope = columns.next()?;
    if !safe_segment(scope) {
        return None;
    }
    let reference = format!("app/{id}/{arch}/{branch}");
    Some(Application {
        key: format!("flatpak:{scope}:{reference}"),
        name: if name.is_empty() { id } else { name }.to_owned(),
        source: if scope == "user" {
            "Flatpak · User"
        } else {
            "Flatpak · System"
        },
        version: columns.next().unwrap_or_default().to_owned(),
        id: id.to_owned(),
        summary: String::new(),
        origin: format!("{reference} ({scope})"),
        launcher: None,
        removable: true,
        launcher_removable: false,
        icon: id.to_owned(),
        install: Install::Flatpak {
            reference,
            scope: scope.to_owned(),
        },
    })
}

pub fn parse_ref(reference: &str) -> Option<(&str, &str, &str)> {
    let parts: [&str; 3] = reference
        .strip_prefix("app/")
        .unwrap_or(reference)
        .split('/')
        .collect::<Vec<_>>()
        .try_into()
        .ok()?;
    let [id, arch, branch] = parts;
    (identifier(id) && safe_segment(arch) && safe_segment(branch)).then_some((id, arch, branch))
}

fn rpm_owners(cancel: &AtomicBool) -> Result<BTreeMap<PathBuf, (String, String)>, String> {
    // Query the database in one process. Limit retained output to application launchers.
    let script = "[%{FILENAMES}\t%{=NAME}\t%{=ARCH}\t%{=VERSION}-%{=RELEASE}\n]";
    let mut child = Command::new("/usr/bin/rpm")
        .args(["-qa", "--qf", script])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    use std::io::BufRead;
    let mut map = BTreeMap::new();
    let result = (|| {
        let pipe = child.stdout.take().ok_or("Could not read RPM inventory")?;
        for line in io::BufReader::new(pipe).lines() {
            check_cancel(cancel)?;
            let line = line.map_err(|e| e.to_string())?;
            if !line.starts_with("/usr/share/applications/") {
                continue;
            }
            if let [path, package, arch, version] = line.split('\t').collect::<Vec<_>>().as_slice()
                && path.ends_with(".desktop")
                && rpm_name(package)
                && safe_segment(arch)
            {
                map.insert(
                    PathBuf::from(path),
                    (format!("{package}.{arch}"), (*version).to_owned()),
                );
            }
        }
        Ok::<_, String>(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    result?;
    if !status.success() {
        return Err(format!("RPM inventory exited with {status}."));
    }
    Ok(map)
}

pub fn scan(locations: &Locations, cancel: &AtomicBool) -> Result<Inventory, String> {
    let mut inventory = Inventory::default();
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| env::var(name).ok())
        .unwrap_or_default();
    let locale = locale.split('.').next().unwrap_or("");
    for root in &locations.desktop {
        if let Err(error) = walk_desktop(root, root, 0, locale, &mut inventory, cancel) {
            check_cancel(cancel)?;
            inventory.warnings.push(error);
        }
    }
    check_cancel(cancel)?;
    match flatpaks() {
        Ok((apps, warnings)) => {
            inventory.applications = apps;
            inventory.flatpak_complete = warnings.is_empty();
            inventory.warnings.extend(warnings);
        }
        Err(error) => inventory
            .warnings
            .push(format!("Flatpak inventory unavailable: {error}")),
    }
    for manager in [Manager::Snap, Manager::Pipx, Manager::Cargo] {
        check_cancel(cancel)?;
        match manager.inventory() {
            Ok((apps, warning)) => {
                inventory.applications.extend(apps);
                inventory.managed_warnings.extend(warning);
            }
            Err(error) => inventory.managed_warnings.push(error),
        }
    }
    check_cancel(cancel)?;
    let owners = match rpm_owners(cancel) {
        Ok(owners) => {
            inventory.rpm_complete = true;
            owners
        }
        Err(error) => {
            check_cancel(cancel)?;
            inventory
                .warnings
                .push(format!("RPM inventory unavailable: {error}"));
            BTreeMap::new()
        }
    };
    let dnf_available = Path::new("/usr/bin/dnf5").is_file();
    let mut visited_manual = BTreeSet::new();
    let mut tracked_images = BTreeSet::new();
    let snap_launchers = Path::new(managed::SNAP_EXPORT).join("applications");
    for entry in &mut inventory.launchers {
        check_cancel(cancel)?;
        if entry.path.starts_with(&snap_launchers) {
            if !entry.hidden
                && let Some(app) = inventory.applications.iter_mut().find(|app| {
                    app.source == "Snap" && app.id == entry.snap && app.launcher.is_none()
                })
            {
                app.name.clone_from(&entry.name);
                app.summary.clone_from(&entry.summary);
                app.launcher = Some(entry.path.clone());
                app.icon.clone_from(&entry.icon);
            }
            continue;
        }
        entry.rpm = owners.get(&entry.path).cloned();
        if !entry.flatpak.is_empty() {
            for app in inventory
                .applications
                .iter_mut()
                .filter(|app| app.id == entry.flatpak && app.launcher.is_none())
            {
                let user_export = entry
                    .path
                    .starts_with(locations.data.join("flatpak/exports"));
                if app.source == "Flatpak · User" && !user_export
                    || app.source == "Flatpak · System" && user_export
                {
                    continue;
                }
                app.summary = entry.summary.clone();
                app.launcher = Some(entry.path.clone());
                if !entry.icon.is_empty() {
                    app.icon = entry.icon.clone();
                }
            }
            continue;
        }
        if entry.hidden {
            continue;
        }
        let program = executable(&entry.exec).and_then(|value| resolve(&value));
        let (install, source, version, origin, removable) =
            if let Some((spec, version)) = &entry.rpm {
                let name = spec
                    .rsplit_once('.')
                    .map_or(spec.as_str(), |(name, _)| name);
                (
                    Install::Rpm(spec.clone()),
                    "RPM",
                    version.clone(),
                    spec.clone(),
                    dnf_available && !protected_rpm(name),
                )
            } else if let Some(path) = program.as_deref().filter(|path| is_appimage(path)) {
                if !tracked_images.insert(path.to_owned()) {
                    continue;
                }
                let removable = image_integration_path(locations, path);
                (
                    Install::AppImage(path.to_owned()),
                    "AppImage",
                    entry.appimage_version.clone(),
                    path.display().to_string(),
                    removable,
                )
            } else if inventory.rpm_complete
                && entry.path
                    == locations
                        .data
                        .join("applications/jetbrains-toolbox.desktop")
                && let Some(bin) = program
                    .as_deref()
                    .filter(|path| path.ends_with("bin/jetbrains-toolbox"))
                    .and_then(Path::parent)
                && crate::cleanup::verified_toolbox_bin(locations, bin)
            {
                (
                    Install::Toolbox(bin.to_owned()),
                    "Manual",
                    String::new(),
                    bin.display().to_string(),
                    true,
                )
            } else if inventory.rpm_complete
                && let Some(path) = program.as_deref()
                && crate::cleanup::verified_standalone(locations, entry, path)
            {
                (
                    Install::Standalone(path.to_owned()),
                    "Manual",
                    String::new(),
                    path.display().to_string(),
                    true,
                )
            } else {
                if entry.path.starts_with("/usr/share/applications") && !inventory.rpm_complete {
                    continue;
                }
                if program.is_none()
                    && !entry.exec.is_empty()
                    && (!entry.conditional
                        || !entry.try_exec.is_empty() && resolve(&entry.try_exec).is_none())
                {
                    continue;
                }
                if !visited_manual.insert(entry.id.clone()) {
                    continue;
                }
                (
                    Install::Manual,
                    "Manual",
                    String::new(),
                    entry.path.display().to_string(),
                    false,
                )
            };
        inventory.applications.push(Application {
            key: format!("{source}:{}:{}", entry.id, entry.path.display()),
            name: entry.name.clone(),
            source,
            version,
            id: entry.id.clone(),
            summary: entry.summary.clone(),
            origin,
            launcher: Some(entry.path.clone()),
            removable,
            launcher_removable: source == "Manual"
                && crate::cleanup::user_launcher_removable(locations, &entry.path),
            icon: entry.icon.clone(),
            install,
        });
    }
    for directory in locations.appimage_dirs() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                inventory
                    .warnings
                    .push(format!("{}: {e}", directory.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            check_cancel(cancel)?;
            let path = entry.path();
            if tracked_images.contains(&path)
                || !path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("appimage"))
                || !is_appimage(&path)
            {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            inventory.applications.push(Application {
                key: format!("AppImage:{}", path.display()),
                name: name.to_owned(),
                source: "AppImage",
                version: String::new(),
                id: String::new(),
                summary: String::new(),
                origin: path.display().to_string(),
                launcher: None,
                removable: image_integration_path(locations, &path),
                launcher_removable: false,
                icon: String::new(),
                install: Install::AppImage(path),
            });
        }
    }
    if !inventory.rpm_complete {
        inventory
            .applications
            .iter_mut()
            .filter(|app| app.install == Install::Manual)
            .for_each(|app| app.removable = false);
    }
    Ok(inventory)
}

pub fn image_integration_path(locations: &Locations, path: &Path) -> bool {
    path.starts_with(&locations.home)
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("appimage"))
        && locations
            .appimage_dirs()
            .iter()
            .any(|dir| path.parent() == Some(dir))
        && fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_launcher_and_safe_exec_subset() {
        let entry = launcher("/tmp/x.desktop".into(), "x".into(),
            "[Desktop Entry]\nType=Application\nName=Example\nName[fr]=Exemple\nExec=env FLAG=1 \"/home/user/My App.AppImage\" %U\nTryExec=/home/user/optional\n[Desktop Action Other]\nName=Ignore\n", "fr_FR").unwrap();
        assert_eq!(entry.name, "Exemple");
        assert_eq!(
            executable(&entry.exec).as_deref(),
            Some("/home/user/My App.AppImage")
        );
        assert!(entry.conditional);
        for command in [
            "env PATH=/fake missing",
            "env -S script",
            "relative/path",
            "%u",
            "a;bad",
        ] {
            assert!(executable(command).is_none());
        }
    }

    #[test]
    fn flatpak_records_allow_missing_versions() {
        let app = flatpak_record("org.example.Test/x86_64/stable\tTest\tuser").unwrap();
        assert_eq!(app.version, "");
        assert_eq!(app.source, "Flatpak · User");
        let app = flatpak_record("org.example.Test/x86_64/stable\tTest\tcustom\t3.2").unwrap();
        assert_eq!(app.version, "3.2");
        assert!(app.origin.ends_with("(custom)"));
    }

    #[test]
    fn parses_full_flatpak_refs_without_patterns() {
        assert_eq!(
            parse_ref("app/org.example.Test/x86_64/stable"),
            Some(("org.example.Test", "x86_64", "stable"))
        );
        for reference in [
            "org.example.Test",
            "org.example.*/x86_64/stable",
            "runtime/org.example.Test/x86_64/stable",
        ] {
            assert!(parse_ref(reference).is_none());
        }
    }
}
