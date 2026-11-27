# Application Cleanup

A desktop uninstaller for Fedora. Remove RPMs, Flatpaks, AppImages, and supported manual installs. Move leftover files and broken launchers to Recovery.

## Install

Download the [Fedora RPM](https://radnotred.github.io/dnf-uninstaller/rpm/x86_64/application-cleanup-0.1.6-1.x86_64.rpm) and [signing key](https://radnotred.github.io/dnf-uninstaller/RPM-GPG-KEY-application-cleanup.asc).

Check the key fingerprint. It should be `9D5F E8B7 282D 258E 10D1 D1DA C758 D9F9 9751 9514`.

```sh
gpg --show-keys --fingerprint RPM-GPG-KEY-application-cleanup.asc
sudo rpmkeys --import RPM-GPG-KEY-application-cleanup.asc
sudo dnf5 --setopt=localpkg_gpgcheck=1 install ./application-cleanup-0.1.6-1.x86_64.rpm
```

Check for new versions under **Settings → Software updates**.

## Build

Install Rust and Bun, then run the following on Fedora.

```sh
sudo dnf5 install gcc openssl-devel webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel rpm-build desktop-file-utils polkit
bun ci
bun run build
```

The RPM is in `src-tauri/target/release/bundle/rpm/`. Run `bun run dev` for development.
