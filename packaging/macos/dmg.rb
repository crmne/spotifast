# frozen_string_literal: true

# native-packages supplies an owned copy, already signed when Apple credentials
# are configured. Keep it unchanged and package only this input.
require "fileutils"
require "tmpdir"

payload, output = ARGV
abort "usage: dmg.rb PAYLOAD OUTPUT.dmg" unless payload && output
abort "output already exists: #{output}" if File.exist?(output)
Dir.mktmpdir("spotizgeg-dmg-") do |directory|
  FileUtils.cp_r(File.join(payload, "."), directory, preserve: true)
  app = File.join(directory, "Spotizgeg.app")
  abort "expected the signed Spotizgeg.app input" unless File.directory?(app)
  File.symlink("/Applications", File.join(directory, "Applications"))
  abort "DMG creation failed" unless system("hdiutil", "create", "-volname", "Spotizgeg",
    "-srcfolder", directory, "-format", "UDZO", output)
  abort "DMG verification failed" unless system("hdiutil", "verify", output)
end
