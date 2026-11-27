# Application Cleanup

Application Cleanup is a Linux desktop uninstaller built with Rust and Tauri 2. Fedora and GNOME are the primary targets.

- **Applications** lists RPM-owned desktop applications, user and system Flatpaks, AppImages, and identifiable manual installations. Search by name or ID and filter by source.
- **Uninstall** previews the RPMs in a DNF5 transaction, the exact Flatpak ref, or the AppImage and matching user launchers. Package removal keeps application data available for separate review.
- **GNOME Launchers** reports missing executables and unavailable icons. Confirmed stale launchers in your user applications directory can be moved to Recovery. Package-owned launchers, conditional shortcuts and icon-only issues remain available for inspection.
- **Leftovers** checks Flatpak data directories, exact desktop IDs recorded after removal, and user icons linked to stale launchers. Review the full path and evidence before selecting a candidate.
- **Recovery** restores moved items to their original paths when those paths are free. Deleting from Recovery frees disk space and requires a separate confirmation.

## Build on Fedora

Install the current stable Rust toolchain, Node.js and npm, then install Fedora's build prerequisites:

```sh
sudo dnf5 install gcc openssl-devel webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel rpm-build desktop-file-utils flatpak polkit
npm ci
npm run build
```

Install the generated local RPM as an administrator and launch **Application Cleanup** from the app menu as your regular user. The local build is unsigned.

```sh
sudo dnf5 install "./src-tauri/target/release/bundle/rpm/Application Cleanup-0.1.6-1.$(uname -m).rpm"
```

For development, run `npm run dev`. The Fedora RPM installs the root-owned executable used by PolicyKit. A development build can inspect DNF5 removal plans. It can apply them when the installed PolicyKit helper is available.

## Removal and recovery

- **RPM.** The application previews a remove-only DNF5 transaction from the installed package database with automatic dependency cleanup disabled. The PolicyKit helper regenerates the plan, compares the exact package list and gives the DNF5 replay command the regenerated transaction. A changed plan requires another preview. Each removal has its own confirmation. PolicyKit can briefly reuse administrator authorization for RPM removals during an active local session.
- **Flatpak.** Removal targets one fully qualified application ref in its selected installation. System installations use Flatpak's own PolicyKit integration. Runtimes, related extensions, and per-user application data are left for separate management.
- **AppImage and manual installs.** AppImages recognized by their file header can be removed from `~/Applications`, `~/AppImages`, `~/.local/bin`, and `$XDG_DATA_HOME/appimages` when the location is inside your home directory. Other integrated AppImages and manual desktop applications are listed for inspection. The custom JetBrains Toolbox uninstaller moves verified Toolbox program files and matching user launchers to Recovery. It preserves installed IDEs and Toolbox data. Standalone apps such as Horizon can be removed if a matching user launcher points directly to a single executable in a dedicated folder. Recovery keeps the executable and launcher restorable. A matching standalone binary in `~/.local/bin` can also be removed. All manual applications with an eligible user-owned desktop entry offer a separate **Remove launcher** action.
- **File cleanup.** The scanner uses exact attribution and inspects system launcher entries for diagnostics. File cleanup operates on supported user-owned paths inside your home directory. Recovery uses `$XDG_DATA_HOME/application-cleanup-recovery` and a same-filesystem rename. If the item and Recovery are on different filesystems, the item stays in place. The default data directory is `~/.local/share`.
- **Desktop menu.** GNOME observes changes to desktop entries. After moving or restoring a launcher, the app also refreshes desktop MIME associations with `update-desktop-database`.
- **Settings.** Appearance is stored in `$XDG_CONFIG_HOME/application-cleanup/settings.json`. System appearance follows GNOME's color-scheme setting, with WebKit's system preference as a fallback. Removal history is stored in `$XDG_STATE_HOME/application-cleanup/history.json`.

## Checks

```sh
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
npm run format:check
npm run lint:js
npm run build
```

## Software updates

A signed Fedora release installs a DNF repository and its public key. Application Cleanup checks that repository on launch and shows available upgrades in **Settings → Software updates**. You can also check manually. Review the version and confirm installation. DNF verifies the repository and the RPM, and Fedora handles administrator authentication. Reopen the app after updating.

Local builds and ordinary CI artifacts are unsigned. They show the update setup status in Settings. Install the signed RPM from GitHub Releases or the public update site to enroll in updates. Verify the release key fingerprint before first installation. The signing fingerprint for `RadNotRed/dnf-uninstaller` is `9D5F E8B7 282D 258E 10D1 D1DA C758 D9F9 9751 9514`. Then import the key and install the RPM with local signature checking enabled.

```sh
gpg --show-keys --fingerprint RPM-GPG-KEY-application-cleanup.asc
sudo rpmkeys --import RPM-GPG-KEY-application-cleanup.asc
sudo dnf5 --setopt=localpkg_gpgcheck=1 install ./application-cleanup-0.1.6-1.x86_64.rpm
```

Use `reinstall` in that command if you already have an unsigned build of the same version.

## GitHub Actions and releases

Pushes and pull requests run Prettier, the JavaScript syntax check, Rust formatting, Clippy, tests, and PolicyKit XML validation. The Fedora 45 x86_64 job builds an RPM and uploads it as a workflow artifact. Dependabot checks Rust, npm, and Actions dependencies weekly.

Configure signed releases once in the GitHub repository.

1. In **Settings → Pages**, select **GitHub Actions** as the publishing source. GitHub Pages hosts the signed DNF repository.
2. Create a dedicated OpenPGP release signing key that can sign unattended. Keep a secure backup and publish its public fingerprint where users can verify it.
3. In **Settings → Secrets and variables → Actions**, add the armored public key as the repository variable `RPM_SIGNING_PUBLIC_KEY` and the armored private signing key as the repository secret `RPM_SIGNING_PRIVATE_KEY`. Keep the same key for future releases. Use protected release tags and restrict the `github-pages` environment to release tags.
4. Keep `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json` at the same version, update `Cargo.lock`, then push the matching tag, such as `v0.1.6`. The workflow builds and signs the RPM and repository metadata, deploys Pages, and publishes the GitHub Release with the RPM, public key, and checksum.
