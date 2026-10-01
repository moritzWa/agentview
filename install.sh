#!/usr/bin/env bash

set -euo pipefail

repo="${AGENTVIEW_REPO:-moritzWa/agentview}"
requested_version="${AGENTVIEW_VERSION:-latest}"
install_dir="${AGENTVIEW_INSTALL_DIR:-${HOME}/.local/bin}"
release_base_url="${AGENTVIEW_RELEASE_BASE_URL:-}"
github_api_url="${AGENTVIEW_GITHUB_API_URL:-https://api.github.com}"

say() {
  printf 'agentview: %s\n' "$*"
}

fail() {
  printf 'agentview: error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Install agentview from a verified GitHub release.

Usage: install.sh [OPTIONS]

Options:
  --version VERSION      Install a specific version (for example, 0.1.12)
  --install-dir DIR      Install directory (default: ~/.local/bin)
  --repo OWNER/REPO      GitHub repository (default: moritzWa/agentview)
  -h, --help             Show this help

Environment variables:
  AGENTVIEW_VERSION            Version to install, or "latest"
  AGENTVIEW_INSTALL_DIR        Installation directory
  AGENTVIEW_REPO               GitHub repository
  GH_TOKEN               Optional token for API rate limits or a private fork

The installer downloads a prebuilt archive and verifies its SHA-256 checksum.
It installs agentview plus the av shorthand symlink.
It never installs Rust, invokes Cargo, or edits shell configuration files.
EOF
}

while (($#)); do
  case "$1" in
    --version)
      (($# >= 2)) || fail "--version requires a value"
      requested_version="$2"
      shift 2
      ;;
    --install-dir)
      (($# >= 2)) || fail "--install-dir requires a value"
      install_dir="$2"
      shift 2
      ;;
    --repo)
      (($# >= 2)) || fail "--repo requires a value"
      repo="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1 (try --help)"
      ;;
  esac
done

[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
  fail "repository must have the form OWNER/REPO"
[[ -n "$install_dir" ]] || fail "installation directory cannot be empty"

for command in curl tar install ln mktemp mv readlink; do
  command -v "$command" >/dev/null 2>&1 || fail "required command not found: $command"
done

os="${_AGENTVIEW_TEST_UNAME_S:-$(uname -s)}"
architecture="${_AGENTVIEW_TEST_UNAME_M:-$(uname -m)}"

case "${os}/${architecture}" in
  Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
  Darwin/x86_64 | Darwin/amd64) target="x86_64-apple-darwin" ;;
  Darwin/arm64 | Darwin/aarch64) target="aarch64-apple-darwin" ;;
  *) fail "no prebuilt release is available for ${os}/${architecture}; see docs/install.md for supported platforms" ;;
esac

curl_args=(--fail --silent --show-error --location --retry 3 --proto '=https,file' --tlsv1.2)
if [[ -n "${GH_TOKEN:-}" ]]; then
  curl_args+=(--header "Authorization: Bearer ${GH_TOKEN}")
  curl_args+=(--header 'X-GitHub-Api-Version: 2022-11-28')
fi

if [[ "$requested_version" == "latest" ]]; then
  release_json="$(curl "${curl_args[@]}" "${github_api_url}/repos/${repo}/releases/latest")" ||
    fail "no release found for ${repo}; set GH_TOKEN only when using a private fork or encountering API rate limits"
  tag="$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [[ -n "$tag" ]] || fail "the latest release response did not contain a tag"
else
  tag="v${requested_version#v}"
fi

version="${tag#v}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  fail "release version must have the form MAJOR.MINOR.PATCH (received: ${tag})"

IFS=. read -r version_major version_minor version_patch <<<"$version"
version_major=$((10#$version_major))
version_minor=$((10#$version_minor))
version_patch=$((10#$version_patch))
linux_only_release=false
if ((version_major == 0 && version_minor == 1 && version_patch >= 13 && version_patch <= 45)); then
  linux_only_release=true
fi
if [[ "$linux_only_release" == true && "$target" != "x86_64-unknown-linux-gnu" ]]; then
  fail "v${version} was manually published only for Linux x86_64; use a source build on ${target} or install a release that provides that target"
fi

stem="agentview-${version}-${target}"
archive="${stem}.tar.gz"
checksum="${archive}.sha256"

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/agentview-install.XXXXXX")"
staged_binary=""
cleanup() {
  if [[ -n "$staged_binary" && -e "$staged_binary" ]]; then
    rm -f -- "$staged_binary"
  fi
  rm -rf -- "$temp_dir"
}
trap cleanup EXIT HUP INT TERM

say "downloading ${tag} for ${target}"
if [[ -n "$release_base_url" ]]; then
  base="${release_base_url%/}/${tag}"
  curl "${curl_args[@]}" --output "${temp_dir}/${archive}" "${base}/${archive}"
  curl "${curl_args[@]}" --output "${temp_dir}/${checksum}" "${base}/${checksum}"
else
  base="https://github.com/${repo}/releases/download/${tag}"
  curl "${curl_args[@]}" --output "${temp_dir}/${archive}" "${base}/${archive}"
  curl "${curl_args[@]}" --output "${temp_dir}/${checksum}" "${base}/${checksum}"
fi

expected_checksum="$(awk 'NR == 1 { print $1 }' "${temp_dir}/${checksum}")"
[[ "$expected_checksum" =~ ^[0-9a-fA-F]{64}$ ]] || fail "release checksum file is malformed"

if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum="$(sha256sum "${temp_dir}/${archive}" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual_checksum="$(shasum -a 256 "${temp_dir}/${archive}" | awk '{ print $1 }')"
else
  fail "sha256sum or shasum is required to verify the release"
fi

[[ "$actual_checksum" == "$expected_checksum" ]] || fail "release checksum verification failed"

tar -xzf "${temp_dir}/${archive}" -C "$temp_dir"
binary="${temp_dir}/${stem}/agentview"
[[ -f "$binary" && -x "$binary" ]] || fail "release archive does not contain ${stem}/agentview"

install -d "$install_dir"
staged_binary="${install_dir}/.agentview.install.$$"
install -m 0755 "$binary" "$staged_binary"
mv -f -- "$staged_binary" "${install_dir}/agentview"
staged_binary=""

installed_version="$("${install_dir}/agentview" --version 2>/dev/null)" ||
  fail "the installed binary could not be executed"
[[ "$installed_version" == "agentview ${version}" ]] ||
  fail "installed binary reported an unexpected version: ${installed_version}"

install_alias() {
  local alias="$1"
  local destination="${install_dir}/${alias}"
  if [[ -e "$destination" || -L "$destination" ]]; then
    local replace_existing=false
    if [[ -L "$destination" && "$(readlink "$destination")" == "agentview" ]]; then
      replace_existing=true
    fi
    if [[ "$replace_existing" != true ]]; then
      say "left unrelated existing command in place: ${destination}"
      return
    fi
  fi
  staged_binary="${install_dir}/.${alias}.install.$$"
  ln -s "agentview" "$staged_binary"
  mv -f -- "$staged_binary" "$destination"
  staged_binary=""
}

install_alias av

say "installed agentview ${version} to ${install_dir}/agentview"
say "installed shorthand: av"
case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) say "add ${install_dir} to PATH, then run: agentview (or av)" ;;
esac
