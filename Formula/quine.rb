class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  url "https://github.com/runtian-zhou/quine.git",
      tag: "v0.2.9",
      revision: "8bbd97a57fa00e67c8bb3599b4f995f2ccb5a86b"
  version "0.2.9"
  license "MIT OR Apache-2.0"
  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "crates/quine-cli"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
