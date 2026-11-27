#!/usr/bin/env bash
set -euo pipefail

mode=${1:?Specify prepare or sign.}
output=${2:?Specify the build output directory.}
mkdir -p "$output"
output=$(realpath "$output")
config="$output/config"
public_key="$config/RPM-GPG-KEY-application-cleanup"

case "$mode" in
  prepare)
    : "${RPM_SIGNING_PUBLIC_KEY:?Configure the RPM_SIGNING_PUBLIC_KEY repository variable.}"
    : "${UPDATE_BASE_URL:?Set the HTTPS GitHub Pages URL.}"
    mkdir -p "$config"
    printf '%s\n' "$RPM_SIGNING_PUBLIC_KEY" > "$public_key"
    key_count=$(gpg --batch --with-colons --show-keys "$public_key" | awk -F: '$1 == "pub" { count++ } END {print count+0}')
    test "$key_count" -eq 1
    python3 - "$config" "$UPDATE_BASE_URL" <<'PY'
import json
import re
import sys
from pathlib import Path

directory = Path(sys.argv[1])
url = sys.argv[2].rstrip("/")
if not re.fullmatch(r"https://[a-zA-Z0-9.-]+(/[a-zA-Z0-9._-]+)*", url):
    raise SystemExit("The update repository must use an HTTPS URL with a simple path.")
repo = directory / "application-cleanup.repo"
repo.write_text(f"""[application-cleanup]
name=Application Cleanup
baseurl={url}/rpm/$basearch
enabled=1
includepkgs=application-cleanup
pkg_gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///usr/share/pki/rpm-gpg/RPM-GPG-KEY-application-cleanup
auto_import_local_keys=1
skip_if_unavailable=1
sslverify=1
metadata_expire=1h
""")
files = {
    "/etc/yum.repos.d/application-cleanup.repo": str(repo),
    "/usr/share/pki/rpm-gpg/RPM-GPG-KEY-application-cleanup": str(directory / "RPM-GPG-KEY-application-cleanup"),
}
(directory / "tauri.release.json").write_text(json.dumps({"bundle": {"linux": {"rpm": {"files": files}}}}))
PY
    ;;
  sign)
    : "${RPM_SIGNING_PRIVATE_KEY:?Configure the RPM_SIGNING_PRIVATE_KEY repository secret.}"
    rpm=${3:?Specify the built RPM.}
    export GNUPGHOME
    GNUPGHOME=$(mktemp -d)
    trap 'gpgconf --kill gpg-agent; rm -rf -- "$GNUPGHOME"' EXIT
    chmod 700 "$GNUPGHOME"
    printf '%s\n' "$RPM_SIGNING_PRIVATE_KEY" | gpg --batch --import
    unset RPM_SIGNING_PRIVATE_KEY
    fingerprint=$(gpg --batch --with-colons --show-keys "$public_key" | awk -F: '$1 == "fpr" && !printed { print $10; printed=1 }')
    gpg --batch --list-secret-keys "$fingerprint" >/dev/null
    IFS=$'\t' read -r name version release arch < <(rpm -qp --qf '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n' "$rpm")
    test "$name" = application-cleanup
    test "$release" = 1
    [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
    [[ "$arch" =~ ^(x86_64|aarch64)$ ]]
    site="$output/site/rpm/$arch"
    assets="$output/assets"
    mkdir -p "$assets" "$site"
    filename="$name-$version-$release.$arch.rpm"
    cp -- "$rpm" "$assets/$filename"
    rpmsign --define '_openpgp_sign gpg' --define "_openpgp_sign_id $fingerprint" --addsign "$assets/$filename"

    # Verify with only the public key shipped in the updater.
    mkdir "$GNUPGHOME/rpmdb"
    rpmkeys --dbpath "$GNUPGHOME/rpmdb" --import "$public_key"
    rpmkeys --dbpath "$GNUPGHOME/rpmdb" --checksig "$assets/$filename"
    cp -- "$assets/$filename" "$site/$filename"
    createrepo_c "$site"
    gpg --batch --local-user "$fingerprint" --armor --detach-sign "$site/repodata/repomd.xml"
    gpg --batch --verify "$site/repodata/repomd.xml.asc" "$site/repodata/repomd.xml"
    cp -- "$public_key" "$assets/RPM-GPG-KEY-application-cleanup.asc"
    (cd "$assets" && sha256sum -- "$filename" > "$filename.sha256")
    cp -- "$assets/RPM-GPG-KEY-application-cleanup.asc" "$output/site/"
    cp -- "$assets/$filename.sha256" "$site/"
    ;;
  *)
    printf 'Unknown release command %s\n' "$mode" >&2
    exit 2
    ;;
esac
