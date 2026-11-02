# frozen_string_literal: true

# native-packages supplies an owned copy, already signed when Apple credentials
# are configured. Keep it unchanged and package only this input.
require "fileutils"
require "find"
require "tmpdir"

payload, output = ARGV
abort "usage: dmg.rb PAYLOAD OUTPUT.dmg" unless payload && output
abort "output already exists: #{output}" if File.exist?(output)
Dir.mktmpdir("spotifast-dmg-") do |directory|
  FileUtils.cp_r(File.join(payload, "."), directory, preserve: true)
  app = File.join(directory, "Spotifast.app")
  abort "expected the signed Spotifast.app input" unless File.directory?(app)
  File.symlink("/Applications", File.join(directory, "Applications"))
  # hdiutil's own estimate for -srcfolder can come up short and fail with
  # "No space left on device", so the working image gets a stated size.
  megabytes = Find.find(directory).sum { |path| File.lstat(path).size } / (1024 * 1024)
  size = "#{(megabytes * 3 / 2) + 64}m"
  unless system("hdiutil", "create", "-volname", "Spotifast", "-srcfolder", directory,
    "-format", "UDZO", "-size", size, output)
    system("df", "-h")
    abort "DMG creation failed"
  end
  abort "DMG verification failed" unless system("hdiutil", "verify", output)
end
