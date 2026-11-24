#!/bin/sh
# okilum-launcher v1
app=/Applications/Okilum.app
if [ "$#" -eq 1 ] && [ "$1" = "--version" ]; then
    version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist") || exit
    printf 'okilum %s\n' "$version"
    exit
fi
exec "$app/Contents/MacOS/okilum" "$@"
