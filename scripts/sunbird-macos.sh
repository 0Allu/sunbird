#!/bin/sh
# Installs Sunbird for Desktop Phoenix on macOS (Apple silicon): a Sunbird app
# that starts Discord with the loader. For now it only opens Discord's access
# gate; plugins don't run on macOS yet.
#
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-macos.sh | sh
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-macos.sh | sh -s -- "/path/to/Discord Phoenix.app"
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-macos.sh | sh -s -- --uninstall
#
# Discord Phoenix.app runs under the hardened runtime, which ignores
# DYLD_INSERT_LIBRARIES, so Sunbird starts an ad-hoc signed copy of it and
# leaves the original untouched. SUNBIRD_SITE points it at a local server for
# testing.
set -eu

site=${SUNBIRD_SITE:-https://assets.sunbird.tools}
home="$HOME/Library/Application Support/Sunbird"
app="$HOME/Applications/Sunbird.app"

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" "$app"
   echo "Sunbird is uninstalled. Discord Phoenix itself was not changed."
   exit 0
fi

if [ "$(uname -m)" != arm64 ]; then
   echo "Discord Phoenix for macOS runs on Apple silicon only." >&2
   exit 1
fi

# What the site serves is loaded into Discord, so only https, or plain http to
# this machine for testing.
case $site in
   https://*) proto==https ;;
   http://127.0.0.1 | http://127.0.0.1[:/]* | http://localhost | http://localhost[:/]*) proto==http ;;
   *)
      echo "SUNBIRD_SITE must be https:// or a localhost server, not $site" >&2
      exit 1
      ;;
esac

script="curl -fsSL $site/plugins/sunbird-macos.sh | sh -s --"
discord=${1:-}
if [ -z "$discord" ]; then
   for candidate in "/Applications/Discord Phoenix.app" "$HOME/Applications/Discord Phoenix.app"; do
      if [ -d "$candidate" ]; then
         discord=$candidate
         break
      fi
   done
fi
if [ -z "$discord" ] || [ ! -x "$discord/Contents/MacOS/Discord" ]; then
   echo "Couldn't find Discord Phoenix.app. Install it, or pass its path:" >&2
   echo "  $script \"/path/to/Discord Phoenix.app\"" >&2
   exit 1
fi
discord=$(cd "$discord" && pwd)
echo "Using Discord Phoenix at $discord"

# Single-quotes a value for the generated launcher.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

mkdir -p "$home" "$app/Contents/MacOS"
# The launcher brings the loader up to the site's version, refreshes the
# signed copy when Discord updates, and starts it with the loader. Offline, it
# keeps the loader it has.
cat >"$home/sunbird" <<EOF
#!/bin/sh
home=$(q "$home")
site=$(q "$site")
discord=$(q "$discord")
copy="\$home/Discord Phoenix.app"
fetch() { curl -fsSL --proto $proto --proto-redir =https "\$@" 2>/dev/null; }
version=\$(fetch --max-time 5 "\$site/version.json" |
   sed -n 's/.*"loader": *"\([^"]*\)".*/\1/p')
if [ -n "\$version" ] && [ "\$version" != "\$(cat "\$home/version" 2>/dev/null)" ]; then
   tmp=\$(mktemp "\$home/libphoenix.dylib.XXXXXX")
   if fetch --max-time 60 -o "\$tmp" "\$site/plugins/libphoenix-macos-arm64.dylib" &&
      codesign --force --sign - "\$tmp" >/dev/null 2>&1; then
      mv "\$tmp" "\$home/libphoenix.dylib"
      echo "\$version" >"\$home/version"
   else
      rm -f "\$tmp"
   fi
fi
[ "\${1:-}" = --update ] && exit 0
# A Discord update replaces its binary, so the copy follows it. Signing drops
# the hardened runtime, which would ignore the loader.
stamp=\$(stat -f %m "\$discord/Contents/MacOS/Discord")
if [ ! -x "\$copy/Contents/MacOS/Discord" ] || [ "\$stamp" != "\$(cat "\$home/discord.stamp" 2>/dev/null)" ]; then
   rm -rf "\$copy"
   cp -R "\$discord" "\$copy"
   xattr -cr "\$copy"
   codesign --force --sign - "\$copy" >/dev/null 2>&1 || exit 1
   echo "\$stamp" >"\$home/discord.stamp"
fi
# The original would take over as the running Discord, so it is closed first.
pkill -f "\$discord/Contents/MacOS/Discord" 2>/dev/null && sleep 1
[ -f "\$home/libphoenix.dylib" ] &&
   export DYLD_INSERT_LIBRARIES="\$home/libphoenix.dylib\${DYLD_INSERT_LIBRARIES:+:\$DYLD_INSERT_LIBRARIES}"
exec "\$copy/Contents/MacOS/Discord" "\$@"
EOF
chmod +x "$home/sunbird"

# A Sunbird app in ~/Applications, so Spotlight, Launchpad and the Dock find it.
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
   <key>CFBundleName</key><string>Sunbird</string>
   <key>CFBundleIdentifier</key><string>tools.sunbird.launcher</string>
   <key>CFBundleExecutable</key><string>sunbird</string>
   <key>CFBundleIconFile</key><string>Sunbird</string>
   <key>CFBundlePackageType</key><string>APPL</string>
</dict>
</plist>
EOF
cat >"$app/Contents/MacOS/sunbird" <<EOF
#!/bin/sh
exec $(q "$home/sunbird") "\$@"
EOF
chmod +x "$app/Contents/MacOS/sunbird"
icon=$(ls "$discord/Contents/Resources/"*.icns 2>/dev/null | head -n 1 || true)
if [ -n "$icon" ]; then
   mkdir -p "$app/Contents/Resources"
   cp "$icon" "$app/Contents/Resources/Sunbird.icns"
fi

# First download now, so a missing network shows up here rather than as a
# Discord without Sunbird.
echo "Downloading Sunbird..."
rm -f "$home/version"
"$home/sunbird" --update
if [ -f "$home/libphoenix.dylib" ]; then
   echo "Sunbird is installed. Open \"Sunbird\" from Applications to start Discord with it."
else
   echo "Installed, but downloading Sunbird failed. It will retry each time you open it." >&2
fi
echo "It runs a copy of $discord: if you move it, run this again."
