class Fad < Formula
  desc "Upload, download, and install APK/AAB releases on Firebase App Distribution"
  homepage "https://github.com/ntsk/fad"
  version "@@VERSION@@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/ntsk/fad/releases/download/v@@VERSION@@/fad-v@@VERSION@@-aarch64-apple-darwin.tar.gz"
      sha256 "@@SHA_DARWIN_ARM@@"
    end
    on_intel do
      url "https://github.com/ntsk/fad/releases/download/v@@VERSION@@/fad-v@@VERSION@@-x86_64-apple-darwin.tar.gz"
      sha256 "@@SHA_DARWIN_INTEL@@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/ntsk/fad/releases/download/v@@VERSION@@/fad-v@@VERSION@@-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@@SHA_LINUX_ARM@@"
    end
    on_intel do
      url "https://github.com/ntsk/fad/releases/download/v@@VERSION@@/fad-v@@VERSION@@-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@@SHA_LINUX_INTEL@@"
    end
  end

  def install
    bin.install "fad"
  end

  test do
    assert_match "fad #{version}", shell_output("#{bin}/fad --version")
  end
end
