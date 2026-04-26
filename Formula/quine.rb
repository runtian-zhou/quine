class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  url "https://github.com/runtian-zhou/quine.git",
      tag: "v0.2.4",
      revision: "b5ed42ed6f8339bdfb9ad037315000df21662289"
  version "0.2.4"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
