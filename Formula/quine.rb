class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  url "https://github.com/runtian-zhou/quine.git",
      tag: "v0.2.7",
      revision: "89909c49e701ae6fbb6c15a606f9fe2a2fdac36a"
  version "0.2.7"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
