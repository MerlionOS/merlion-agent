class MerlionAgent < Formula
  desc "Self-improving AI coding agent — Rust port of hermes-agent"
  homepage "https://github.com/MerlionOS/merlion-agent"
  version "0.1.7"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-apple-darwin.tar.gz"
      sha256 "e081c1b3f30b83661f784331fef256f40e00e69636910415025fafcfe6bca4b5"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-apple-darwin.tar.gz"
      sha256 "352b59967dd6b7edad00ef3976e8365c44363cc1e4ed54ddc8ab69dc94076a71"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0154e20f3cb109935efc6c93f3d0dfd7bf2e33cd3ba4943d74eb0673c4745d9a"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7aa7848bf9deb627c8a124948b05d2b909712a6ba2a89d23251d0d905b0895f7"
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
