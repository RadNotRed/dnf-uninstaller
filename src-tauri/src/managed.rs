use crate::{
    inventory::{Application, Install, resolve},
    packages::{Preview, command_error, run_with_progress},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const SNAP: &str = "/usr/bin/snap";
const PKEXEC: &str = "/usr/bin/pkexec";
pub const SNAP_EXPORT: &str = "/var/lib/snapd/desktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Snap,
    Pipx,
    Cargo,
}

impl Manager {
    fn source(self) -> &'static str {
        match self {
            Self::Snap => "Snap",
            Self::Pipx => "pipx",
            Self::Cargo => "Cargo",
        }
    }

    pub fn inventory(self) -> Result<(Vec<Application>, Option<String>), String> {
        match self.binary() {
            Some(binary) => self.list(&binary),
            None => Ok((Vec::new(), None)),
        }
    }

    fn binary(self) -> Option<PathBuf> {
        match self {
            Self::Snap if Path::new("/run/snapd.socket").exists() => resolve(SNAP),
            Self::Snap => None,
            Self::Pipx => resolve("pipx"),
            Self::Cargo => resolve("cargo"),
        }
    }

    fn list(self, binary: &Path) -> Result<(Vec<Application>, Option<String>), String> {
        let args: &[&str] = match self {
            Self::Snap => &["debug", "api", "/v2/snaps"],
            Self::Pipx => &["list", "--json"],
            Self::Cargo => &["install", "--list", "--offline"],
        };
        let label = format!("{} inventory", self.source());
        let output = Command::new("/usr/bin/timeout")
            .args(["--kill-after=1s", "8s"])
            .arg(binary)
            .args(args)
            .current_dir("/")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("{label} could not start: {error}"))?;
        let partial_pipx = self == Self::Pipx && output.status.code() == Some(1);
        if !output.status.success() && !partial_pipx {
            return Err(if self == Self::Pipx {
                // pipx's machine-readable output can include private package URLs.
                format!("{label} failed ({}).", output.status)
            } else {
                command_error(&label, &output)
            });
        }
        let apps = self
            .parse(&output.stdout)
            .map_err(|_| format!("{label} returned an unreadable package list."))?;
        let warning = partial_pipx.then(|| {
            "pipx reported problems while listing environments. Run pipx list for details.".into()
        });
        Ok((apps, warning))
    }

    fn parse(self, output: &[u8]) -> Result<Vec<Application>, ()> {
        Ok(match self {
            Self::Snap => {
                let response: SnapList = serde_json::from_slice(output).map_err(|_| ())?;
                if response.status_code != 200 {
                    return Err(());
                }
                response
                    .result
                    .into_iter()
                    .filter(|snap| {
                        // Content runtimes can also have type "app" and run internal daemons.
                        snap.kind == "app" && snap.apps.iter().any(|app| app.daemon.is_empty())
                    })
                    .filter_map(|snap| self.app(&snap.name, &snap.version, &snap.revision))
                    .collect()
            }
            Self::Pipx => {
                let response: PipxList = serde_json::from_slice(output).map_err(|_| ())?;
                response
                    .venvs
                    .iter()
                    .filter_map(|(name, venv)| {
                        self.app(name, &venv.metadata.main_package.package_version, "")
                    })
                    .collect()
            }
            Self::Cargo => std::str::from_utf8(output)
                .map_err(|_| ())?
                .lines()
                .filter(|line| line.ends_with(':'))
                .filter_map(|line| {
                    let (name, version) = line.split_once(" v")?;
                    let version = version.trim_end_matches(':').split_whitespace().next()?;
                    self.app(name, version, "")
                })
                .collect(),
        })
    }

    fn app(self, name: &str, version: &str, revision: &str) -> Option<Application> {
        if !name.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_')
                    || self == Self::Pipx && matches!(byte, b'.' | b'@' | b'+')
            })
        {
            return None;
        }
        let source = self.source();
        Some(Application {
            key: format!("{source}:{name}"),
            name: name.into(),
            source,
            version: version.into(),
            id: name.into(),
            summary: String::new(),
            origin: name.into(),
            launcher: None,
            removable: self != Self::Snap || Path::new(PKEXEC).is_file(),
            launcher_removable: false,
            icon: String::new(),
            install: Install::Managed {
                manager: self,
                package: name.into(),
                revision: revision.into(),
            },
        })
    }

    fn checked_binary(self, selected: &Application) -> Result<PathBuf, String> {
        let binary = self
            .binary()
            .ok_or_else(|| format!("{} is unavailable.", self.source()))?;
        let (apps, _) = self.list(&binary)?;
        let mut matching = apps.iter().filter(|app| app.key == selected.key);
        if matching.next().is_some_and(|app| {
            app.removable
                && app.id == selected.id
                && app.version == selected.version
                && app.install == selected.install
        }) && matching.next().is_none()
        {
            Ok(binary)
        } else {
            Err(
                "The selected package changed or is unavailable. Refresh the list and try again."
                    .into(),
            )
        }
    }

    pub fn preview(self, selected: &Application) -> Result<Preview, String> {
        self.checked_binary(selected)?;
        let explanation = match self {
            Self::Snap => {
                "Snap will remove all revisions of this package and its managed data. Snapd normally saves a data snapshot."
            }
            Self::Pipx => {
                "pipx will remove this environment, including any injected packages and its exposed commands."
            }
            Self::Cargo => "Cargo will remove all binaries installed by this crate.",
        };
        Ok(Preview {
            items: vec![selected.id.clone()],
            explanation: explanation.into(),
            authorization: (self == Self::Snap)
                .then_some("PolicyKit may ask for administrator authentication."),
        })
    }

    pub fn apply(self, selected: &Application, progress: impl FnMut(String)) -> Result<(), String> {
        let binary = self.checked_binary(selected)?;
        let mut command = match self {
            Self::Snap => {
                let mut command = Command::new(PKEXEC);
                command.args(["--disable-internal-agent", SNAP, "remove", "--"]);
                command
            }
            Self::Pipx | Self::Cargo => {
                let mut command = Command::new(binary);
                command.args(["uninstall", "--"]);
                command
            }
        };
        command.current_dir("/").arg(&selected.id);
        run_with_progress(command, &format!("{} removal", self.source()), progress)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SnapList {
    status_code: u16,
    result: Vec<SnapRecord>,
}

#[derive(Deserialize)]
struct SnapRecord {
    name: String,
    version: String,
    revision: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    apps: Vec<SnapApp>,
}

#[derive(Deserialize)]
struct SnapApp {
    #[serde(default)]
    daemon: String,
}

#[derive(Deserialize)]
struct PipxList {
    venvs: BTreeMap<String, PipxEnvironment>,
}

#[derive(Deserialize)]
struct PipxEnvironment {
    metadata: PipxMetadata,
}

#[derive(Deserialize)]
struct PipxMetadata {
    main_package: PipxPackage,
}

#[derive(Deserialize)]
struct PipxPackage {
    package_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_sources_keep_exact_removal_targets() {
        let snaps = Manager::Snap
            .parse(
                br#"{"status-code":200,"result":[
            {"name":"core22","version":"1","revision":"12","type":"base"},
                {"name":"mesa","version":"1","revision":"8","type":"app","apps":[{"daemon":"simple"}]},
                {"name":"demo_work","version":"2 beta","revision":"x7","type":"app","apps":[{"name":"demo"}]},
                {"name":"--purge","version":"1","revision":"5","type":"app","apps":[{"name":"bad"}]}
        ]}"#,
            )
            .unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, "demo_work");
        assert_eq!(snaps[0].version, "2 beta");
        assert!(
            matches!(&snaps[0].install, Install::Managed { manager: Manager::Snap, revision, .. } if revision == "x7")
        );

        let pipx = Manager::Pipx
            .parse(
                br#"{"venvs":{"black@22":{"metadata":{
            "main_package":{"package":"black","package_version":"22.1.0"}
        }}}}"#,
            )
            .unwrap();
        assert_eq!(pipx[0].id, "black@22");
        assert_eq!(pipx[0].version, "22.1.0");

        let cargo = Manager::Cargo
            .parse(b"ripgrep v14.1.1:\n    rg\nmy_tool v0.1.0 (/tmp/source):\n    my-tool\n")
            .unwrap();
        assert_eq!(cargo.len(), 2);
        assert_eq!(cargo[1].id, "my_tool");
        assert_eq!(cargo[1].version, "0.1.0");
    }
}
