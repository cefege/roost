# Homebrew formula for Roost. Publish via a tap (e.g. cefege/homebrew-tap):
#   brew install cefege/tap/roost
# Per release, bump `version` and set `sha256` from the release's roost.sha256
# (the release workflow uploads it). depends_on "tailscale" installs the
# open-source tailscaled — no System Settings network-extension approval.
#
# macOS-only ON PURPOSE: `roost` (unsuffixed) is the darwin-arm64 asset, and
# there is no tested linuxbrew bottle. Linux installs go through
# install-binary.sh, which resolves roost-linux-{x64,arm64} into ~/.local/bin
# and needs no package manager. Dropping depends_on :macos here would advertise
# an install path nobody has verified.
class Roost < Formula
  desc "Run Claude Code on all your machines from one browser tab"
  homepage "https://github.com/cefege/roost"
  version "0.2.0"
  url "https://github.com/cefege/roost/releases/download/v#{version}/roost"
  sha256 "106827de6fb11ee003e899697e29b2e05eb677eab22d8a0f457b7bf9ba4a074b"
  license "GPL-3.0-only"

  depends_on "tailscale"
  depends_on :macos

  def install
    bin.install "roost"
  end

  def caveats
    <<~EOS
      Start Tailscale (the brew CLI daemon needs no System Settings approval):
        sudo tailscaled install-system-daemon && sudo tailscale up
      Then bring Roost up on this machine:
        roost quickstart
    EOS
  end

  test do
    assert_match(/\d+\.\d+|dev/, shell_output("#{bin}/roost version"))
  end
end
