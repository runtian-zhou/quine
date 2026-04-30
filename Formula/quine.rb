class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  url "https://github.com/runtian-zhou/quine.git",
      tag: "v0.2.6",
      revision: "3d473c60473e0600e8cae6e3dfa478f4df3867d5"
  version "0.2.6"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
