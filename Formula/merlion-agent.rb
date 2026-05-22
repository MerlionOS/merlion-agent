class MerlionAgent < Formula
  desc "Self-improving AI coding agent — Rust port of hermes-agent"
  homepage "https://github.com/MerlionOS/merlion-agent"
  version "0.1.14"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-apple-darwin.tar.gz"
      sha256 "af9240b5584007a34fe27517dc35184690b0daa84bbbcd4f28c6328fbd8493cb"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-apple-darwin.tar.gz"
      sha256 "fbed6af19c6955fcb61d9f189a1e8ae957813c7ef65ee5d2342fbe04386855d0"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3ac74527d3d8bdd1afbe99524b26899a518926d9e0a2c51c99d8f264ee9585bf"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ce927bb6e84b8fe43dff049c2b77b999278c6bb14ef747dc79e8aadcc1870127"
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
