class MerlionAgent < Formula
  desc "Self-improving AI coding agent — Rust port of hermes-agent"
  homepage "https://github.com/MerlionOS/merlion-agent"
  version "0.1.12"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-apple-darwin.tar.gz"
      sha256 "1bac6f5ed4b9443b8f126bab4cb17eb019fcb45bf5adfc5678c88c88e92a573b"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-apple-darwin.tar.gz"
      sha256 "7c2f589288f83a7f44bf70fc4f6ad531e2439095348589f2832be5de18965d25"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "950d48ba64f216fa58a83d46336e840ee16deafc8ee37a7ca08f001cb2fddcb1"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "09538de95a3db2c2ed6502d57c9a8f061dc93bbcac207d81cc9881f0562e327d"
    end
  end

  def install
    # The release workflow tars binaries as merlion-<target>/merlion, but
    # Homebrew auto-strips the single top-level directory at extraction
    # time, so by the time we run we're already inside the merlion-<target>
    # dir and the binary is at the staging root.
    bin.install "merlion"
  end

  test do
    assert_match "merlion-agent", shell_output("#{bin}/merlion --version")
  end
end
