class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  url "https://github.com/runtian-zhou/quine.git",
      tag: "v0.2.5",
      revision: "d8b0829966d2686b30fedcbf0276f5a7c641a14a"
  version "0.2.5"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
