class Merlion < Formula
  desc "Self-improving AI coding agent — Rust port of hermes-agent"
  homepage "https://github.com/MerlionOS/merlion-agent"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-apple-darwin.tar.gz"
      sha256 "0fbaa52c76953e5f315433a2bbcf903fe7d118e9604021d027e110ee50367ae7"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-apple-darwin.tar.gz"
      sha256 "8fae33815b1c3e4f57b9bf6e89ec6a572507c774c16378e21a5ba720edb6537e"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5ef9f927a5094e3d4d15f36833d76ab645bc4f4cad399ac285b89336709ed8c3"
    else
      url "https://github.com/MerlionOS/merlion-agent/releases/download/v#{version}/merlion-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1bfc830c8d5829b11ebe95b771f6a8fe7b32c0f76d9ecbc70904236e6f96bdbd"
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
