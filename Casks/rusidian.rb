cask "rusidian" do
  version :latest
  sha256 :no_check

  url "https://github.com/AlbertHuangT/rusidian/releases/download/nightly/Rusidian.dmg"
  name "Rusidian"
  desc "Local-first native Markdown with real Neovim"
  homepage "https://github.com/AlbertHuangT/rusidian"

  auto_updates true
  depends_on arch: :arm64
  depends_on :macos

  app "Rusidian.app"

  zap trash: [
    "~/Library/Application Support/rusidian",
    "~/Library/Caches/rusidian",
  ]
end
