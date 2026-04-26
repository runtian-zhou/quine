#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifacts-dir> <output-dir>" >&2
  exit 1
fi

artifacts_dir="$1"
output_dir="$2"
release_tag="${RELEASE_TAG:?RELEASE_TAG must be set}"
release_version="${release_tag#v}"
repo_slug="${GITHUB_REPOSITORY:-runtian-zhou/quine}"

mkdir -p "$output_dir"

shopt -s nullglob
release_files=("$artifacts_dir"/quine-*.tar.gz)
if [[ ${#release_files[@]} -eq 0 ]]; then
  echo "no release archives found in $artifacts_dir" >&2
  exit 1
fi

cp "${release_files[@]}" "$output_dir"/

(
  cd "$output_dir"
  shasum -a 256 quine-*.tar.gz | sort -k2 > SHA256SUMS
)

checksum_for() {
  local archive_name="$1"
  awk -v target="$archive_name" '$2 == target { print $1 }' "$output_dir/SHA256SUMS"
}

linux_x86_64_archive="quine-x86_64-linux-${release_tag}.tar.gz"
macos_x86_64_archive="quine-x86_64-macos-${release_tag}.tar.gz"
macos_arm64_archive="quine-aarch64-macos-${release_tag}.tar.gz"

linux_x86_64_sha="$(checksum_for "$linux_x86_64_archive")"
macos_x86_64_sha="$(checksum_for "$macos_x86_64_archive")"
macos_arm64_sha="$(checksum_for "$macos_arm64_archive")"

for checksum_name in \
  linux_x86_64_sha \
  macos_x86_64_sha \
  macos_arm64_sha
do
  if [[ -z "${!checksum_name}" ]]; then
    echo "missing checksum for ${checksum_name}" >&2
    exit 1
  fi
done

cat > "$output_dir/quine.rb" <<EOF
class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/${repo_slug}"
  version "${release_version}"
  license "MIT OR Apache-2.0"

  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/${repo_slug}/releases/download/${release_tag}/${macos_arm64_archive}"
    sha256 "${macos_arm64_sha}"
  elsif OS.mac?
    url "https://github.com/${repo_slug}/releases/download/${release_tag}/${macos_x86_64_archive}"
    sha256 "${macos_x86_64_sha}"
  elsif OS.linux? && Hardware::CPU.intel?
    url "https://github.com/${repo_slug}/releases/download/${release_tag}/${linux_x86_64_archive}"
    sha256 "${linux_x86_64_sha}"
  end

  def install
    bin.install "quine"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
EOF

echo "Generated release metadata in $output_dir"
