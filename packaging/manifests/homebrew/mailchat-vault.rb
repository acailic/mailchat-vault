# BLOCKED — DO NOT SUBMIT until signing lands (#78).
#
# Mailchat Vault installers are currently unsigned (ad-hoc macOS
# signature only): Gatekeeper blocks the first launch of a downloaded
# copy, and Homebrew must not silently install an artifact the OS
# warns about. Keep this cask unsubmitted; it exists so publication is
# one review away once owner signing (#78) ships signed dmgs.
# scripts/update-manifests.sh rewrites version and sha256 from a
# release's SHA256SUMS (see docs/distribution.md).
cask "mailchat-vault" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/acailic/mailchat-vault/releases/download/v#{version}/mailchat-vault_#{version}_universal.dmg"
  name "Mailchat Vault"
  desc "Local email & chat archive with full-text search"
  homepage "https://github.com/acailic/mailchat-vault"

  # In-app auto-update is deferred by decision (D11): upgrades are fresh
  # installs, which Homebrew itself handles. Flip this to true only if
  # the app ever ships its own updater — a stale `true` would stop
  # Homebrew from offering upgrades.
  auto_updates false

  app "mailchat-vault.app"

  zap trash: [
    "~/Library/Application Support/com.acailic.mailchatvault",
    "~/Library/Caches/com.acailic.mailchatvault",
    "~/Library/Preferences/com.acailic.mailchatvault.plist",
    "~/Library/Saved Application State/com.acailic.mailchatvault.savedState",
  ]
end
