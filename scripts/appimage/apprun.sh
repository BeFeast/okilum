#!/bin/sh
# Keep the host's ALSA ABI and plugin directory together. The bundled loader
# remains a startup fallback for systems without an installed audio stack.
set -eu
app_dir=${APPDIR:-$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)}
for native_alsa in \
    /usr/lib/x86_64-linux-gnu/libasound.so.2 \
    /usr/lib64/libasound.so.2 \
    /usr/lib/libasound.so.2
do
    if [ -r "$native_alsa" ]; then
        LD_PRELOAD="$native_alsa${LD_PRELOAD:+:$LD_PRELOAD}"
        export LD_PRELOAD
        break
    fi
done
exec "$app_dir/usr/bin/okilum" "$@"
