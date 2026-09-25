class Unpackr < Formula
  desc "Low-disk-space archive extraction engine and native GUI"
  homepage "https://github.com/RahulKumar-007/Unpackr"
  url "https://github.com/RahulKumar-007/Unpackr/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0019dfc4b32d63c1392aa264aed2253c1e0c2fb09216f8e2cc269bbfb8bb49b5"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/RahulKumar-007/Unpackr.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    generate_completions_from_executable(bin/"unpackr", "completions")
  end

  test do
    assert_match "unpackr #{version}", shell_output("#{bin}/unpackr --version")
  end
end
