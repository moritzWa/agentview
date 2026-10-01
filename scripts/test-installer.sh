#!/usr/bin/env bash

set -euo pipefail

repo_dir="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/agentview-installer-test.XXXXXX")"
trap 'rm -rf -- "$temp_root"' EXIT HUP INT TERM

current_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${repo_dir}/Cargo.toml" | head -n 1)"
[[ -n "$current_version" ]] || {
  printf 'installer tests could not read the current package version\n' >&2
  exit 1
}
version="$current_version"
tag="v${version}"
release_dir="${temp_root}/releases/${tag}"

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) host_target="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64 | Linux/arm64) host_target="aarch64-unknown-linux-gnu" ;;
  Darwin/x86_64 | Darwin/amd64) host_target="x86_64-apple-darwin" ;;
  Darwin/arm64 | Darwin/aarch64) host_target="aarch64-apple-darwin" ;;
  *) printf 'installer tests require a supported release host\n' >&2; exit 1 ;;
esac
host_stem="agentview-${version}-${host_target}"
host_archive="${host_stem}.tar.gz"

fail() {
  printf 'installer test failed: %s\n' "$*" >&2
  exit 1
}

package_binary="${temp_root}/package-binary"
cat >"$package_binary" <<EOF
#!/usr/bin/env sh
if [ "\${1:-}" = "--version" ]; then
  echo "agentview ${version}"
  exit 0
fi
echo fixture-binary
EOF
chmod 0755 "$package_binary"
package_dist="${temp_root}/package-dist"
AGENTVIEW_DIST_DIR="$package_dist" \
  "${repo_dir}/scripts/package-release.sh" "$host_target" "$package_binary" >/dev/null
[[ -f "${package_dist}/${host_archive}" ]] || fail "native release archive was not packaged"
[[ -f "${package_dist}/${host_archive}.sha256" ]] || fail "native release checksum was not packaged"
(
  cd "$package_dist"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "${host_archive}.sha256" >/dev/null
  else
    shasum -a 256 -c "${host_archive}.sha256" >/dev/null
  fi
)
tar -tzf "${package_dist}/${host_archive}" | grep -F "${host_stem}/agentview" >/dev/null ||
  fail "native release archive is missing the executable"

make_release() {
  local root="$1"
  local target="$2"
  local stem="agentview-${version}-${target}"
  local archive="${stem}.tar.gz"
  install -d "${root}/${stem}"
  cat >"${root}/${stem}/agentview" <<EOF
#!/usr/bin/env sh
if [ "\${1:-}" = "--version" ]; then
  echo "agentview ${version}"
  exit 0
fi
echo fixture-binary
EOF
  chmod 0755 "${root}/${stem}/agentview"
  tar -C "$root" -czf "${root}/${archive}" "$stem"
  (
    cd "$root"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$archive" >"${archive}.sha256"
    else
      shasum -a 256 "$archive" >"${archive}.sha256"
    fi
  )
}

install -d "$release_dir"
for target in \
  x86_64-unknown-linux-gnu \
  aarch64-unknown-linux-gnu \
  x86_64-apple-darwin \
  aarch64-apple-darwin; do
  make_release "$release_dir" "$target"
done

home="${temp_root}/home"
output="$({
  HOME="$home" \
    PATH="/usr/bin:/bin" \
    AGENTVIEW_VERSION="$version" \
    AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
    bash "${repo_dir}/install.sh"
} 2>&1)"
[[ -x "${home}/.local/bin/agentview" ]] || fail "default binary was not installed"
[[ "$("${home}/.local/bin/agentview" --version)" == "agentview ${version}" ]] ||
  fail "default binary reports the wrong version"
[[ -L "${home}/.local/bin/av" ]] || fail "av shorthand was not installed"
[[ "$("${home}/.local/bin/av" --version)" == "agentview ${version}" ]] ||
  fail "av shorthand reports the wrong version"
[[ "$output" == *"installed agentview ${version}"* ]] || fail "success output is missing"
[[ "$output" == *"installed shorthand: av"* ]] || fail "shorthand output is missing"
[[ "$output" == *"add ${home}/.local/bin to PATH"* ]] || fail "PATH guidance is missing"

custom_bin="${temp_root}/custom/bin"
PATH="${custom_bin}:/usr/bin:/bin" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" --version "v${version}" --install-dir "$custom_bin" >/dev/null
[[ -x "${custom_bin}/agentview" ]] || fail "custom install directory was ignored"

collision_bin="${temp_root}/collision/bin"
install -d "$collision_bin"
cat >"${collision_bin}/av" <<'EOF'
#!/usr/bin/env sh
echo unrelated-agentview
EOF
chmod 0755 "${collision_bin}/av"
collision_output="$(AGENTVIEW_VERSION="$version" \
  AGENTVIEW_INSTALL_DIR="$collision_bin" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh")"
[[ "$("${collision_bin}/av")" == "unrelated-agentview" ]] ||
  fail "installer replaced an unrelated av command"
[[ "$collision_output" == *"left unrelated existing command in place"* ]] ||
  fail "shorthand collision was not explained"

printf 'old binary\n' >"${custom_bin}/agentview"
chmod 0755 "${custom_bin}/agentview"
cp "${release_dir}/${host_archive}.sha256" "${release_dir}/${host_archive}.sha256.good"
printf '%064d  %s\n' 0 "$host_archive" >"${release_dir}/${host_archive}.sha256"
if AGENTVIEW_VERSION="$version" \
  AGENTVIEW_INSTALL_DIR="$custom_bin" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" >"${temp_root}/checksum.out" 2>&1; then
  fail "a bad checksum was accepted"
fi
grep -F "checksum verification failed" "${temp_root}/checksum.out" >/dev/null ||
  fail "checksum failure was not explained"
grep -F "old binary" "${custom_bin}/agentview" >/dev/null ||
  fail "failed installation replaced the existing binary"
mv "${release_dir}/${host_archive}.sha256.good" "${release_dir}/${host_archive}.sha256"

if _AGENTVIEW_TEST_UNAME_S=FreeBSD \
  AGENTVIEW_VERSION="$version" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" >"${temp_root}/platform.out" 2>&1; then
  fail "an unsupported platform was accepted"
fi
grep -F "no prebuilt release is available for FreeBSD" "${temp_root}/platform.out" >/dev/null ||
  fail "unsupported platform failure was not explained"

if _AGENTVIEW_TEST_UNAME_S=Darwin \
  _AGENTVIEW_TEST_UNAME_M=arm64 \
  AGENTVIEW_VERSION="0.1.45" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" >"${temp_root}/manual-scope.out" 2>&1; then
  fail "the Linux-only v0.1.45 release accepted a macOS target"
fi
grep -F "v0.1.45 was manually published only for Linux x86_64" \
  "${temp_root}/manual-scope.out" >/dev/null ||
  fail "the v0.1.45 manual platform scope was not explained"

platforms=(
  "Linux x86_64"
  "Linux aarch64"
  "Darwin x86_64"
  "Darwin arm64"
)
for platform in "${platforms[@]}"; do
  read -r test_os test_arch <<<"$platform"
  platform_bin="${temp_root}/platform-${test_os}-${test_arch}/bin"
  _AGENTVIEW_TEST_UNAME_S="$test_os" \
    _AGENTVIEW_TEST_UNAME_M="$test_arch" \
    AGENTVIEW_VERSION="$version" \
    AGENTVIEW_INSTALL_DIR="$platform_bin" \
    AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
    bash "${repo_dir}/install.sh" >/dev/null
  [[ -x "${platform_bin}/agentview" ]] ||
    fail "supported platform mapping failed for ${test_os}/${test_arch}"
done

api_root="${temp_root}/api"
install -d "${api_root}/repos/moritzWa/agentview/releases"
printf '{"tag_name":"%s"}\n' "$tag" >"${api_root}/repos/moritzWa/agentview/releases/latest"
latest_bin="${temp_root}/latest/bin"
HOME="${temp_root}/latest-home" \
  PATH="/usr/bin:/bin" \
  AGENTVIEW_INSTALL_DIR="$latest_bin" \
  AGENTVIEW_GITHUB_API_URL="file://${api_root}" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" >/dev/null
[[ -x "${latest_bin}/agentview" ]] || fail "public latest-release path did not install"

fake_bin="${temp_root}/fake-bin"
install -d "$fake_bin"
cat >"${fake_bin}/gh" <<'EOF'
#!/usr/bin/env bash
printf 'installer unexpectedly invoked gh: %s\n' "$*" >&2
exit 97
EOF
chmod 0755 "${fake_bin}/gh"
public_home="${temp_root}/public-home"
PATH="${fake_bin}:/usr/bin:/bin" \
  HOME="$public_home" \
  AGENTVIEW_GITHUB_API_URL="file://${api_root}" \
  AGENTVIEW_RELEASE_BASE_URL="file://${temp_root}/releases" \
  bash "${repo_dir}/install.sh" >/dev/null
[[ -x "${public_home}/.local/bin/agentview" ]] ||
  fail "public release path did not install without gh"

bash "${repo_dir}/install.sh" --help | grep -F "never installs Rust" >/dev/null ||
  fail "installer help does not state the no-Rust behavior"
bash "${repo_dir}/install.sh" --help | grep -F "av shorthand" >/dev/null ||
  fail "installer help does not advertise the av shorthand"

printf 'installer tests passed\n'
