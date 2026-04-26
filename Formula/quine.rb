class Quine < Formula
  desc "Self-bootstrapping AI agent harness"
  homepage "https://github.com/runtian-zhou/quine"
  version "0.2.0"
  license "MIT OR Apache-2.0"

  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/runtian-zhou/quine/releases/download/v0.2.0/quine-aarch64-macos-v0.2.0.tar.gz"
    sha256 "68ea62b8b0a2b138b2153bab4390b2864b3dbc6eb0f3036e1c7acfb6a6820731"
  elsif OS.mac?
    url "https://github.com/runtian-zhou/quine/releases/download/v0.2.0/quine-x86_64-macos-v0.2.0.tar.gz"
    sha256 "d646504be1da3b00ad9dc1280a8aa160e7e3969af4b149c4bfcca4ba448b237c"
  elsif OS.linux? && Hardware::CPU.intel?
    url "https://github.com/runtian-zhou/quine/releases/download/v0.2.0/quine-x86_64-linux-v0.2.0.tar.gz"
    sha256 "0afdc253b1ce036ad4dcc30fc0b05144e9c9cabe8b1dd82d8f609e331363b717"
  end

  def install
    bin.install "quine"
  end

  test do
    assert_match "quine ", shell_output("#{bin}/quine version")
  end
end
