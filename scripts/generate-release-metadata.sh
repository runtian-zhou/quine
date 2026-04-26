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
release_revision="${RELEASE_REVISION:-$(git rev-parse HEAD)}"

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
  url "https://github.com/${repo_slug}.git",
      tag: "${release_tag}",
      revision: "${release_revision}"
  version "${release_version}"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
EOF

echo "Generated release metadata in $output_dir"
