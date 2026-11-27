use crate::inventory::{parse_ref, protected_rpm, rpm_name, safe_segment};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
};

pub(crate) const INSTALLED_BINARY: &str = "/usr/bin/linux-rust-uninstaller";
const RPM_POLICY: &str =
    "/usr/share/polkit-1/actions/dev.radnotred.application-cleanup.remove-rpm.policy";

#[derive(Debug, Serialize)]
pub struct Preview {
    pub items: Vec<String>,
    pub explanation: String,
    pub authorization: Option<&'static str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    version: String,
    rpms: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    nevra: String,
    action: String,
    reason: String,
    repo_id: String,
}

struct RpmPlan {
    directory: tempfile::TempDir,
    packages: Vec<String>,
}

fn checked_spec(spec: &str) -> Result<(&str, &str), String> {
    let (name, arch) = spec
        .rsplit_once('.')
        .ok_or("Invalid RPM package specification.")?;
    if !rpm_name(name) || !safe_segment(arch) || protected_rpm(name) {
        return Err("This package is protected or its identifier is invalid.".into());
    }
    Ok((name, arch))
}

fn name_arch(nevra: &str) -> Option<(&str, &str)> {
    let (nvr, arch) = nevra.rsplit_once('.')?;
    let (nv, release) = nvr.rsplit_once('-')?;
    let (name, version) = nv.rsplit_once('-')?;
    let valid_version = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'~' | b'^' | b':')
            })
    };
    (rpm_name(name) && safe_segment(arch) && valid_version(version) && valid_version(release))
        .then_some((name, arch))
}

fn validate_transaction(transaction: Transaction, spec: &str) -> Result<Vec<String>, String> {
    let target = checked_spec(spec)?;
    if transaction.version != "1.0" || transaction.rpms.is_empty() || transaction.rpms.len() > 64 {
        return Err("The DNF removal plan is empty, too large, or unsupported.".into());
    }
    let mut packages = Vec::new();
    let mut found = false;
    for item in transaction.rpms {
        let (name, arch) =
            name_arch(&item.nevra).ok_or("DNF returned an invalid package identifier.")?;
        if item.action != "Remove"
            || item.repo_id != "@System"
            || item.reason.is_empty()
            || protected_rpm(name)
        {
            return Err(format!(
                "The transaction includes an unsupported or protected action for {name}."
            ));
        }
        found |= (name, arch) == target;
        packages.push(item.nevra);
    }
    packages.sort();
    if !found || packages.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("The DNF plan does not match the selected package.".into());
    }
    Ok(packages)
}

fn check_desktop_package(spec: &str) -> Result<(), String> {
    checked_spec(spec)?;
    let output = Command::new("/usr/bin/rpm")
        .args(["-ql", "--", spec])
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("RPM could not start: {e}"))?;
    if !output.status.success() {
        return Err("The selected RPM is no longer installed.".into());
    }
    let owns_launcher = String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        if !line.starts_with("/usr/share/applications/") || !line.ends_with(".desktop") {
            return false;
        }
        std::fs::read_to_string(line).is_ok_and(|text| {
            let fields: Vec<_> = text
                .lines()
                .skip_while(|line| line.trim() != "[Desktop Entry]")
                .skip(1)
                .map(str::trim)
                .take_while(|line| !line.starts_with('['))
                .collect();
            fields.contains(&"Type=Application")
                && !fields.contains(&"Hidden=true")
                && !fields.contains(&"NoDisplay=true")
        })
    });
    if !owns_launcher {
        return Err("Only RPMs that own a visible desktop application can be selected.".into());
    }
    Ok(())
}

fn dnf(plan_dir: &Path) -> Command {
    let mut command = Command::new("/usr/bin/dnf5");
    command
        .current_dir("/")
        .env("LC_ALL", "C")
        .args(["--cacheonly", "--color=never", "--assumeyes"])
        // Remove-only transactions resolve against the installed database.
        .arg(format!("--setopt=reposdir={}", plan_dir.display()));
    command
}

fn rpm_plan(spec: &str) -> Result<RpmPlan, String> {
    check_desktop_package(spec)?;
    let directory = tempfile::Builder::new()
        .prefix("application-cleanup-")
        .tempdir_in("/tmp")
        .map_err(|e| e.to_string())?;
    let output = dnf(directory.path())
        .arg(format!("--setopt=logdir={}", directory.path().display()))
        .args(["remove", "--no-autoremove"])
        .arg(format!("--store={}", directory.path().display()))
        .args(["--", spec])
        .output()
        .map_err(|e| format!("DNF5 could not start: {e}"))?;
    if !output.status.success() {
        return Err(command_error("DNF5 preview", &output));
    }
    let path = directory.path().join("transaction.json");
    let data =
        std::fs::read(&path).map_err(|e| format!("DNF5 did not produce a removal plan: {e}"))?;
    let transaction =
        serde_json::from_slice(&data).map_err(|e| format!("Unsupported DNF5 plan: {e}"))?;
    Ok(RpmPlan {
        directory,
        packages: validate_transaction(transaction, spec)?,
    })
}

pub fn preview_rpm(spec: &str) -> Result<Preview, String> {
    let plan = rpm_plan(spec)?;
    Ok(Preview {
        items: plan.packages,
        explanation: "DNF will remove the packages below.".into(),
        authorization: Some("PolicyKit may ask for authentication."),
    })
}

pub fn apply_rpm_as_root(spec: &str, expected: &str) -> Result<(), String> {
    if !rustix::process::geteuid().is_root() {
        return Err("The RPM helper requires PolicyKit authentication.".into());
    }
    if expected.len() > 16_384 {
        return Err("The approved plan is too large.".into());
    }
    let expected: Vec<String> =
        serde_json::from_str(expected).map_err(|_| "Invalid approved plan.")?;
    let plan = rpm_plan(spec)?;
    if plan.packages != expected {
        return Err(
            "The DNF5 removal plan changed. Preview it again before removing packages.".into(),
        );
    }
    let status = dnf(plan.directory.path())
        .arg("replay")
        .arg(plan.directory.path())
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("Could not start DNF5 replay: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("DNF5 replay failed ({status})."))
    }
}

pub fn rpm_helper_available() -> bool {
    use std::os::unix::fs::MetadataExt;
    [INSTALLED_BINARY, RPM_POLICY].iter().all(|file| {
        Path::new(file)
            .symlink_metadata()
            .is_ok_and(|m| m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0)
    }) && Path::new("/usr/bin/pkexec").is_file()
}

pub fn apply_rpm(
    spec: &str,
    approved: &[String],
    progress: impl FnMut(String),
) -> Result<(), String> {
    if !rpm_helper_available() {
        return Err("Install or update the Fedora RPM to enable authenticated removal.".into());
    }
    checked_spec(spec)?;
    let expected = serde_json::to_string(approved).map_err(|e| e.to_string())?;
    let mut command = Command::new("/usr/bin/pkexec");
    command.current_dir("/").args([
        "--disable-internal-agent",
        INSTALLED_BINARY,
        "--apply-rpm",
        spec,
        &expected,
    ]);
    run_with_progress(command, "RPM removal", progress)
}

fn flatpak_scope(scope: &str) -> Result<String, String> {
    match scope {
        "user" => Ok("--user".into()),
        "system" | "default" => Ok("--system".into()),
        value if safe_segment(value) => Ok(format!("--installation={value}")),
        _ => Err("Invalid Flatpak installation.".into()),
    }
}

fn flatpak_app(reference: &str, scope: &str) -> Result<String, String> {
    if !reference.starts_with("app/") || parse_ref(reference).is_none() {
        return Err("Invalid Flatpak app ref.".into());
    }
    let flag = flatpak_scope(scope)?;
    let output = Command::new("/usr/bin/flatpak")
        .args(["info", &flag, "--show-ref", "--", reference])
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("Could not start Flatpak: {e}"))?;
    if !output.status.success() {
        return Err(command_error("Flatpak lookup", &output));
    }
    if String::from_utf8_lossy(&output.stdout).trim() != reference {
        return Err("Flatpak returned a different application than the one selected.".into());
    }
    Ok(flag)
}

pub fn preview_flatpak(reference: &str, scope: &str) -> Result<Preview, String> {
    flatpak_app(reference, scope)?;
    Ok(Preview {
        items: vec![format!("{reference} ({scope})")],
        explanation: "Flatpak will remove this app from the selected installation. Runtimes, related extensions, and personal data stay in place.".into(),
        authorization: (scope != "user").then_some("Flatpak may ask for administrator authentication to remove this system app."),
    })
}

pub fn apply_flatpak(
    reference: &str,
    scope: &str,
    progress: impl FnMut(String),
) -> Result<(), String> {
    let flag = flatpak_app(reference, scope)?;
    let mut command = Command::new("/usr/bin/flatpak");
    // System installations use Flatpak's own system helper and PolicyKit policy.
    command.args([
        "uninstall",
        &flag,
        "--assumeyes",
        "--no-related",
        "--",
        reference,
    ]);
    run_with_progress(command, "Flatpak removal", progress)
}

pub fn command_error(label: &str, output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    format!("{label} failed ({}).\n{detail}", output.status)
}

pub(crate) fn run_with_progress(
    mut command: Command,
    name: &str,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    let mut child = command
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{name} could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("Could not read progress.")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("Could not read package manager errors.")?;
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let progress_result = (|| {
        for line in BufReader::new(stdout).split(b'\n') {
            let line = line.map_err(|e| e.to_string())?;
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if !line.is_empty() {
                progress(line.to_owned());
            }
        }
        Ok::<_, String>(())
    })();
    let status = child.wait().map_err(|e| e.to_string())?;
    let errors = errors
        .join()
        .map_err(|_| "Error reader stopped unexpectedly.")?
        .map_err(|e| e.to_string())?;
    progress_result?;
    if !status.success() {
        return Err(format!(
            "{name} failed ({status}).\n{}",
            String::from_utf8_lossy(&errors).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnf_plans_must_be_removal_only_and_target_exact_package() {
        let make = |nevra: &str, action: &str| Transaction {
            version: "1.0".into(),
            rpms: vec![Action {
                nevra: nevra.into(),
                action: action.into(),
                reason: "User".into(),
                repo_id: "@System".into(),
            }],
        };
        assert!(
            validate_transaction(
                make("some-app-1:2.0-3.fc45.x86_64", "Remove"),
                "some-app.x86_64"
            )
            .is_ok()
        );
        assert!(
            validate_transaction(
                make("some-app-2.0-3.fc45.i686", "Remove"),
                "some-app.x86_64"
            )
            .is_err()
        );
        assert!(
            validate_transaction(
                make("some-app-2.0-3.fc45.x86_64", "Install"),
                "some-app.x86_64"
            )
            .is_err()
        );
        assert!(checked_spec("gnome-shell.x86_64").is_err());
        assert!(checked_spec("app*.x86_64").is_err());
    }

    #[test]
    fn system_and_user_flatpak_scopes_are_explicit() {
        assert_eq!(flatpak_scope("user").unwrap(), "--user");
        assert_eq!(flatpak_scope("system").unwrap(), "--system");
        assert_eq!(flatpak_scope("extra").unwrap(), "--installation=extra");
        assert!(flatpak_scope("--all").is_err());
    }
}
