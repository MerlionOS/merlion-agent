class MerlionAgent < Formula
  desc "Self-improving AI coding agent — Rust port of hermes-agent"
  homepage "https://github.com/MerlionOS/merlion-agent"
  version "0.1.2"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-apple-darwin.tar.gz"
      sha256 "acc90673f55c9acf32598c974910a17fa87132e2fd1c13e9d6f360ef2ab848ac"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-apple-darwin.tar.gz"
      sha256 "f94297abfe400703cdd161f4b4e31d471432ac93bb1a5eafe7904d13d1a16146"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "162b633a1a1208f56f70ed4c74e858b8de6f94f782eb03f235c4d7a96c1605ff"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8e9d837ca64207120152240e45c0a742dc661dea96affcbacfb5b0248eeea077"
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
