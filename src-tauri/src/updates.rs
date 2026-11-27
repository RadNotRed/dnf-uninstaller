use crate::packages::{self, INSTALLED_BINARY, Preview};
use std::{os::unix::fs::MetadataExt, path::Path, process::Command};

const REPO_FILE: &str = "/etc/yum.repos.d/application-cleanup.repo";
const PUBLIC_KEY: &str = "/usr/share/pki/rpm-gpg/RPM-GPG-KEY-application-cleanup";
const PACKAGE: &str = "application-cleanup";

pub fn available() -> bool {
    packages::rpm_helper_available()
        && [REPO_FILE, PUBLIC_KEY].iter().all(|file| {
            Path::new(file).symlink_metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0
            })
        })
}

fn version_parts(version: &str) -> Option<[u64; 3]> {
    if version.len() > 32 {
        return None;
    }
    let mut segments = version.split('.');
    let mut parts = [0; 3];
    for part in &mut parts {
        let text = segments.next()?;
        if !text.bytes().all(|byte| byte.is_ascii_digit())
            || text.len() > 1 && text.starts_with('0')
        {
            return None;
        }
        *part = text.parse().ok()?;
    }
    segments.next().is_none().then_some(parts)
}

fn installed_version() -> Result<String, String> {
    let output = Command::new("/usr/bin/rpm")
        .args(["-q", "--qf", "%{VERSION}\n", "--", PACKAGE])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("Could not start RPM: {error}"))?;
    if !output.status.success() {
        return Err(packages::command_error("Installed version lookup", &output));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    version_parts(&version).ok_or("The installed RPM has an unsupported version.")?;
    Ok(version)
}

fn dnf() -> Command {
    let mut command = Command::new("/usr/bin/dnf5");
    command.current_dir("/").env("LC_ALL", "C").args([
        "--repo=application-cleanup", "--color=never", "--assumeyes",
        "--setopt=application-cleanup.includepkgs=application-cleanup",
        "--setopt=application-cleanup.pkg_gpgcheck=1",
        "--setopt=application-cleanup.repo_gpgcheck=1",
        "--setopt=application-cleanup.gpgkey=file:///usr/share/pki/rpm-gpg/RPM-GPG-KEY-application-cleanup",
        "--setopt=application-cleanup.skip_if_unavailable=0",
        "--setopt=application-cleanup.timeout=10",
        "--setopt=application-cleanup.retries=1",
    ]);
    command
}

pub fn check(refresh: bool) -> Result<Option<String>, String> {
    if !available() {
        return Err("Install the signed Fedora release to enable updates.".into());
    }
    let installed = installed_version()?;
    let logs = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut command = dnf();
    if refresh {
        command.arg("--refresh");
    }
    let output = command
        .arg(format!("--setopt=logdir={}", logs.path().display()))
        .args([
            "repoquery",
            "--upgrades",
            "--queryformat=%{version}|%{release}|%{arch}",
            "--",
        ])
        .arg(format!("{PACKAGE}.{}", std::env::consts::ARCH))
        .output()
        .map_err(|error| format!("Could not start the update check: {error}"))?;
    if !output.status.success() {
        return Err(packages::command_error("Update check", &output));
    }
    select_update(
        &String::from_utf8_lossy(&output.stdout),
        &installed,
        std::env::consts::ARCH,
    )
}

fn select_update(output: &str, installed: &str, arch: &str) -> Result<Option<String>, String> {
    let installed =
        version_parts(installed).ok_or("The installed RPM has an unsupported version.")?;
    let mut latest = None;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.trim().split('|');
        let version = columns.next().unwrap_or_default();
        let Some(parts) = version_parts(version) else {
            return Err("The update repository returned an invalid version.".into());
        };
        if columns.next() != Some("1") || columns.next() != Some(arch) || columns.next().is_some() {
            return Err("The update repository returned an unsupported RPM.".into());
        }
        if parts > installed && latest.as_ref().is_none_or(|(best, _)| parts > *best) {
            latest = Some((parts, version.to_owned()));
        }
    }
    Ok(latest.map(|(_, version)| version))
}

pub fn preview(version: &str) -> Result<Preview, String> {
    if check(false)?.as_deref() != Some(version) {
        return Err("The available update changed. Check for updates again.".into());
    }
    Ok(Preview {
        items: vec![format!("Application Cleanup {version}")],
        explanation:
            "DNF will install the signed update. Reopen Application Cleanup to use the new version."
                .into(),
        authorization: Some("PolicyKit may ask for authentication."),
    })
}

pub fn apply(version: &str, progress: impl FnMut(String)) -> Result<(), String> {
    if !available() || version_parts(version).is_none() {
        return Err("The update is unavailable. Check for updates again.".into());
    }
    let mut command = Command::new("/usr/bin/pkexec");
    command.current_dir("/").args([
        "--disable-internal-agent",
        INSTALLED_BINARY,
        "--apply-update",
        version,
    ]);
    packages::run_with_progress(command, "Update installation", progress)
}

pub fn apply_as_root(version: &str) -> Result<(), String> {
    if !rustix::process::geteuid().is_root() || !available() {
        return Err(
            "An installed update repository and PolicyKit authorization are required.".into(),
        );
    }
    let requested = version_parts(version).ok_or("Invalid update version.")?;
    let installed = &installed_version()?;
    if Some(requested) <= version_parts(installed) {
        return Err("That update is already installed or older than the installed RPM.".into());
    }
    let spec = format!("{PACKAGE}-{version}-1.{}", std::env::consts::ARCH);
    packages::run_with_progress(
        {
            let mut command = dnf();
            command.args(["upgrade", "--", &spec]);
            command
        },
        "DNF update",
        |line| println!("{line}"),
    )?;
    if installed_version()? != version {
        return Err("DNF did not install the reviewed update. Check for updates again.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_versions_are_exact_and_numerically_ordered() {
        assert_eq!(
            select_update("0.1.9|1|x86_64\n0.1.10|1|x86_64\n", "0.1.8", "x86_64")
                .unwrap()
                .as_deref(),
            Some("0.1.10")
        );
        assert_eq!(
            select_update("0.1.9|1|x86_64", "0.1.10", "x86_64").unwrap(),
            None
        );
        for version in ["*", "--all", "0.1.0~rc1", "01.0.0", "0.1", "0.1.2.3"] {
            assert!(version_parts(version).is_none());
        }
        assert!(select_update("0.1.9|1|aarch64", "0.1.8", "x86_64").is_err());
    }
}
