#!/bin/sh
# Installs Sunbird for Desktop Phoenix on Linux: a launcher that preloads the
# loader into Discord, and a desktop entry for it. Discord's own files stay
# untouched, so its updates never undo Sunbird.
#
#   sh sunbird-linux.sh [Discord AppImage or discord-phoenix binary]
#   sh sunbird-linux.sh --uninstall
#
# SUNBIRD_SITE points it at a local server for testing.
set -eu

site=${SUNBIRD_SITE:-https://assets.sunbird.tools}
data=${XDG_DATA_HOME:-$HOME/.local/share}
home=$data/sunbird
entry=$data/applications/sunbird.desktop

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" "$entry"
   echo "Sunbird is uninstalled. Discord itself was not changed."
   exit 0
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

# LD_PRELOAD splits on spaces and colons, and the desktop entry's Exec= would
# need escaping for the rest.
case $home in
   *[[:space:]:\"\$\`\\\']*)
      echo "Sunbird can't be installed under $home: set XDG_DATA_HOME to a path without spaces, colons or quotes." >&2
      exit 1
      ;;
esac

discord=${1:-}
if [ -z "$discord" ]; then
   for found in \
      "$(command -v discord-phoenix 2>/dev/null || true)" \
      "$HOME"/Applications/discord-desktop*.AppImage \
      "$HOME"/Downloads/discord-desktop*.AppImage; do
      if [ -f "$found" ]; then
         discord=$found
         break
      fi
   done
fi
if [ -z "$discord" ] || [ ! -f "$discord" ]; then
   echo "Discord Phoenix not found. Pass its AppImage: sh $0 /path/to/discord-desktop.AppImage" >&2
   exit 1
fi
discord=$(realpath "$discord")
[ -x "$discord" ] || chmod +x "$discord"

# Single-quotes a value for the generated launcher.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

mkdir -p "$home" "$data/applications"
# The launcher brings the loader up to the site's version before each start,
# and keeps the one it has when offline. Each run downloads to its own file, so
# two starts at once never load a half-written one.
cat >"$home/sunbird" <<EOF
#!/bin/sh
home=$(q "$home")
site=$(q "$site")
fetch() { curl -fsSL --proto $proto --proto-redir =https "\$@" 2>/dev/null; }
version=\$(fetch --max-time 5 "\$site/version.json" |
   sed -n 's/.*"loader": *"\([^"]*\)".*/\1/p')
if [ -n "\$version" ] && [ "\$version" != "\$(cat "\$home/version" 2>/dev/null)" ]; then
   tmp=\$(mktemp "\$home/libphoenix.so.XXXXXX")
   if fetch --max-time 60 -o "\$tmp" "\$site/plugins/libphoenix-linux-x86_64.so"; then
      mv "\$tmp" "\$home/libphoenix.so"
      echo "\$version" >"\$home/version"
   else
      rm -f "\$tmp"
   fi
fi
[ "\${1:-}" = --update ] && exit 0
[ -f "\$home/libphoenix.so" ] &&
   export LD_PRELOAD="\$home/libphoenix.so\${LD_PRELOAD:+:\$LD_PRELOAD}"
exec $(q "$discord") "\$@"
EOF
chmod +x "$home/sunbird"

cat >"$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Sunbird
Comment=Discord with Sunbird
Exec="$home/sunbird" %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/discord;
EOF
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

# First download now, so a missing network shows up here rather than as a
# Discord without Sunbird.
rm -f "$home/version"
"$home/sunbird" --update
if [ -f "$home/libphoenix.so" ]; then
   echo "Sunbird installed. Open \"Sunbird\" from your app menu."
else
   echo "Installed, but downloading the loader failed. It will retry on each start." >&2
fi
