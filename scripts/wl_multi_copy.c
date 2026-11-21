/* Own the Wayland CLIPBOARD through wlr-data-control with several MIME types
 * at once, each served from a file, the way Firefox, Chromium, Nautilus,
 * Dolphin or VS Code offer a copy. wl-copy offers one type per selection.
 *
 * Usage: wl_multi_copy MIME=FILE...
 * Logs "send MIME BYTES" to stderr for every transfer a reader asks for, and
 * exits when another client takes the selection. Used by
 * scripts/check-smart-paste-native.py (#1618), which builds it with gcc and
 * wayland-scanner from the wayland-protocols-wlr crate's XML. */
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>

#include "wlr-data-control-unstable-v1-client-protocol.h"

static struct wl_seat *seat;
static struct zwlr_data_control_manager_v1 *manager;
static int count;
static char **mimes, **files;
static int done;

static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface,
                   uint32_t version) {
    if (!strcmp(interface, wl_seat_interface.name) && !seat)
        seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
    if (!strcmp(interface, zwlr_data_control_manager_v1_interface.name))
        manager = wl_registry_bind(registry, name, &zwlr_data_control_manager_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {}

static const struct wl_registry_listener registry_listener = {global, global_remove};

static void send(void *data, struct zwlr_data_control_source_v1 *source, const char *mime, int32_t fd) {
    for (int i = 0; i < count; i++) {
        if (strcmp(mime, mimes[i]))
            continue;
        int in = open(files[i], O_RDONLY);
        char buf[65536];
        ssize_t got;
        size_t total = 0;
        while (in >= 0 && (got = read(in, buf, sizeof buf)) > 0) {
            for (ssize_t off = 0; off < got;) {
                ssize_t put = write(fd, buf + off, got - off);
                if (put <= 0)
                    goto out;
                off += put;
            }
            total += got;
        }
    out:
        if (in >= 0)
            close(in);
        fprintf(stderr, "send %s %zu\n", mime, total);
        break;
    }
    close(fd);
}

static void cancelled(void *data, struct zwlr_data_control_source_v1 *source) {
    fprintf(stderr, "cancelled\n");
    done = 1;
}

static const struct zwlr_data_control_source_v1_listener source_listener = {send, cancelled};

int main(int argc, char **argv) {
    signal(SIGPIPE, SIG_IGN);
    count = argc - 1;
    mimes = calloc(count, sizeof *mimes);
    files = calloc(count, sizeof *files);
    for (int i = 0; i < count; i++) {
        char *eq = strrchr(argv[i + 1], '=');
        if (!eq) {
            fprintf(stderr, "usage: wl_multi_copy MIME=FILE...\n");
            return 2;
        }
        *eq = 0;
        mimes[i] = argv[i + 1];
        files[i] = eq + 1;
    }
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) {
        fprintf(stderr, "no Wayland display\n");
        return 1;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!seat || !manager) {
        fprintf(stderr, "no seat or wlr-data-control\n");
        return 1;
    }
    struct zwlr_data_control_device_v1 *device = zwlr_data_control_manager_v1_get_data_device(manager, seat);
    struct zwlr_data_control_source_v1 *source = zwlr_data_control_manager_v1_create_data_source(manager);
    zwlr_data_control_source_v1_add_listener(source, &source_listener, NULL);
    for (int i = 0; i < count; i++)
        zwlr_data_control_source_v1_offer(source, mimes[i]);
    zwlr_data_control_device_v1_set_selection(device, source);
    wl_display_roundtrip(display);
    fprintf(stderr, "owns %d types\n", count);
    while (!done && wl_display_dispatch(display) != -1) {
    }
    return 0;
}
