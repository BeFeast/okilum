#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "input-method-v2-client.h"

/* A deterministic test input method. No keyboard grab or language engine. */
enum { WIDTH = 260, HEIGHT = 108, MAX_LINE = 4096, MAX_TEXT = 3900 };
struct app;
struct frame {
    struct app *app;
    struct frame *next;
    struct wl_buffer *buffer;
    uint32_t *pixels;
};
struct app {
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct wl_seat *seat;
    struct zwp_input_method_manager_v2 *manager;
    struct zwp_input_method_v2 *ime;
    struct wl_surface *surface;
    struct zwp_input_popup_surface_v2 *popup;
    struct frame *frames;
    uint32_t seat_name, manager_name, serial, sequence;
    unsigned seat_count, manager_count, frame_count, selected_row;
    bool active, pending_active, running, text_input_v3;
    int result;
};

static void json_string(const char *s) {
    putchar('"');
    for (const unsigned char *p = (const unsigned char *)s; *p; ++p) {
        if (*p == '"' || *p == '\\') printf("\\%c", *p);
        else if (*p < 0x20) printf("\\u%04x", *p);
        else putchar(*p);
    }
    putchar('"');
}
static void prefix(struct app *a, const char *event) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    printf("{\"event\":"); json_string(event);
    printf(",\"monotonic_ns\":%llu,\"sequence\":%u,\"serial\":%u,\"active\":%s",
           (unsigned long long)t.tv_sec * 1000000000ULL + (unsigned long long)t.tv_nsec,
           a->sequence, a->serial, a->active ? "true" : "false");
}
static void event(struct app *a, const char *name, const char *message) {
    prefix(a, name);
    printf(",\"text\":"); json_string(message); puts("}");
}
static void fail(struct app *a, const char *message) {
    prefix(a, "failure");
    printf(",\"text\":"); json_string(message);
    if (a->display) {
        int display_error = wl_display_get_error(a->display);
        printf(",\"display_error\":%d", display_error);
        if (display_error == EPROTO) {
            const struct wl_interface *interface = NULL;
            uint32_t object_id = 0;
            uint32_t code = wl_display_get_protocol_error(a->display, &interface, &object_id);
            printf(",\"protocol_code\":%u,\"object_id\":%u,\"interface\":", code, object_id);
            json_string(interface ? interface->name : "unknown");
        }
    }
    puts("}");
    a->result = 1;
    a->running = false;
}
static void expired(int sig) {
    (void)sig;
    static const char message[] = "ime216: bounded runtime expired\n";
    ssize_t written = write(STDERR_FILENO, message, sizeof(message) - 1);
    (void)written;
    _exit(124);
}
static bool valid_utf8(const unsigned char *s) {
    while (*s) {
        uint32_t cp = *s++;
        unsigned more;
        uint32_t minimum;
        if (cp < 0x80) continue;
        if (cp >= 0xc2 && cp <= 0xdf) { more = 1; minimum = 0x80; cp &= 0x1f; }
        else if (cp >= 0xe0 && cp <= 0xef) { more = 2; minimum = 0x800; cp &= 0xf; }
        else if (cp >= 0xf0 && cp <= 0xf4) { more = 3; minimum = 0x10000; cp &= 7; }
        else return false;
        while (more--) {
            if ((*s & 0xc0) != 0x80) return false;
            cp = (cp << 6) | (*s++ & 0x3f);
        }
        if (cp < minimum || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) return false;
    }
    return true;
}

static void shm_format(void *data, struct wl_shm *shm, uint32_t format) {
    (void)data; (void)shm; (void)format;
}
static const struct wl_shm_listener shm_listener = {shm_format};
static void seat_capabilities(void *data, struct wl_seat *seat, uint32_t capabilities) {
    (void)seat;
    struct app *a = data;
    prefix(a, "seat_capabilities"); printf(",\"value\":%u}\n", capabilities);
}
static void seat_name(void *data, struct wl_seat *seat, const char *name) {
    (void)seat;
    event(data, "seat_name", name);
}
static const struct wl_seat_listener seat_listener = {seat_capabilities, seat_name};

static void registry_global(void *data, struct wl_registry *r, uint32_t name,
                            const char *interface, uint32_t version) {
    struct app *a = data;
    prefix(a, "global"); printf(",\"name\":%u,\"version\":%u,\"interface\":", name, version);
    json_string(interface); puts("}");
    if (!strcmp(interface, "wl_compositor") && !a->compositor)
        a->compositor = wl_registry_bind(r, name, &wl_compositor_interface, 1);
    else if (!strcmp(interface, "wl_shm") && !a->shm) {
        a->shm = wl_registry_bind(r, name, &wl_shm_interface, 1);
        wl_shm_add_listener(a->shm, &shm_listener, a);
    }
    else if (!strcmp(interface, "wl_seat")) {
        a->seat_count++;
        if (!a->seat) {
            a->seat_name = name;
            a->seat = wl_registry_bind(r, name, &wl_seat_interface, 1);
            wl_seat_add_listener(a->seat, &seat_listener, a);
        }
    } else if (!strcmp(interface, "zwp_text_input_manager_v3")) {
        a->text_input_v3 = true;
    } else if (!strcmp(interface, "zwp_input_method_manager_v2")) {
        a->manager_count++;
        if (!a->manager) {
            a->manager_name = name;
            a->manager = wl_registry_bind(r, name, &zwp_input_method_manager_v2_interface, 1);
        }
    }
}
static void registry_remove(void *data, struct wl_registry *r, uint32_t name) {
    (void)r;
    struct app *a = data;
    if (name == a->seat_name || name == a->manager_name) fail(a, "required global removed");
}
static const struct wl_registry_listener registry_listener = {registry_global, registry_remove};

static void activate(void *data, struct zwp_input_method_v2 *ime) {
    (void)ime;
    struct app *a = data;
    a->pending_active = true;
    event(a, "activate_pending", "");
}
static void deactivate(void *data, struct zwp_input_method_v2 *ime) {
    (void)ime;
    struct app *a = data;
    a->pending_active = false;
    event(a, "deactivate_pending", "");
}
static void surrounding(void *data, struct zwp_input_method_v2 *ime, const char *text,
                        uint32_t cursor, uint32_t anchor) {
    (void)ime;
    struct app *a = data;
    prefix(a, "surrounding_pending");
    printf(",\"cursor_utf8\":%u,\"anchor_utf8\":%u,\"text\":", cursor, anchor);
    json_string(text); puts("}");
}
static void cause(void *data, struct zwp_input_method_v2 *ime, uint32_t value) {
    (void)ime;
    struct app *a = data;
    prefix(a, "cause_pending"); printf(",\"value\":%u}\n", value);
}
static void content_type(void *data, struct zwp_input_method_v2 *ime, uint32_t hint,
                         uint32_t purpose) {
    (void)ime;
    struct app *a = data;
    prefix(a, "content_type_pending"); printf(",\"hint\":%u,\"purpose\":%u}\n", hint, purpose);
}
static void done(void *data, struct zwp_input_method_v2 *ime) {
    (void)ime;
    struct app *a = data;
    a->serial++;
    a->active = a->pending_active;
    event(a, "done", "");
}
static void unavailable(void *data, struct zwp_input_method_v2 *ime) {
    (void)ime;
    fail(data, "input method unavailable; existing IME will not be replaced");
}
static const struct zwp_input_method_v2_listener ime_listener = {
    activate, deactivate, surrounding, cause, content_type, done, unavailable
};
static void rectangle(void *data, struct zwp_input_popup_surface_v2 *popup,
                      int32_t x, int32_t y, int32_t width, int32_t height) {
    (void)popup;
    struct app *a = data;
    prefix(a, "popup_rectangle");
    printf(",\"x\":%d,\"y\":%d,\"width\":%d,\"height\":%d,\"coordinate_space\":\"popup_surface\"}\n",
           x, y, width, height);
}
static const struct zwp_input_popup_surface_v2_listener popup_listener = {rectangle};

static void free_frame(struct frame *frame) {
    struct app *a = frame->app;
    struct frame **link = &a->frames;
    while (*link && *link != frame) link = &(*link)->next;
    if (*link) *link = frame->next;
    wl_buffer_destroy(frame->buffer);
    munmap(frame->pixels, WIDTH * HEIGHT * 4);
    a->frame_count--;
    free(frame);
}
static void release_frame(void *data, struct wl_buffer *buffer) {
    (void)buffer;
    free_frame(data);
}
static const struct wl_buffer_listener buffer_listener = {release_frame};

/* Tiny fixed pixel labels avoid font/toolkit dependencies in the test popup. */
static const uint8_t *glyph(char c) {
    static const uint8_t blank[7] = {0};
    static const uint8_t i[7] = {31,4,4,4,4,4,31};
    static const uint8_t m[7] = {17,27,21,21,17,17,17};
    static const uint8_t e[7] = {31,16,16,30,16,16,31};
    static const uint8_t n[7] = {17,25,25,21,19,19,17};
    static const uint8_t h[7] = {17,17,17,31,17,17,17};
    static const uint8_t o[7] = {14,17,17,17,17,17,14};
    static const uint8_t one[7] = {4,12,4,4,4,4,14};
    static const uint8_t two[7] = {14,17,1,2,4,8,31};
    static const uint8_t six[7] = {14,16,16,30,17,17,14};
    switch (c) {
        case 'I': return i; case 'M': return m; case 'E': return e;
        case 'N': return n; case 'H': return h; case 'O': return o;
        case '1': return one; case '2': return two; case '6': return six;
        default: return blank;
    }
}
static void label(uint32_t *pixels, int x, int y, const char *text, uint32_t color) {
    for (; *text; ++text, x += 12) {
        const uint8_t *g = glyph(*text);
        for (int row = 0; row < 7; ++row)
            for (int col = 0; col < 5; ++col)
                if (g[row] & (1U << (4 - col)))
                    for (int dy = 0; dy < 2; ++dy)
                        for (int dx = 0; dx < 2; ++dx)
                            pixels[(y + row * 2 + dy) * WIDTH + x + col * 2 + dx] = color;
    }
}
static bool render_popup(struct app *a) {
    if (a->frame_count >= 16) { fail(a, "popup buffer backpressure"); return false; }
    int fd = memfd_create("okilum-ime216-popup", MFD_CLOEXEC);
    if (fd < 0) { fail(a, "memfd_create failed"); return false; }
    if (ftruncate(fd, WIDTH * HEIGHT * 4) < 0) {
        close(fd); fail(a, "ftruncate failed"); return false;
    }
    uint32_t *pixels = mmap(NULL, WIDTH * HEIGHT * 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) { close(fd); fail(a, "mmap failed"); return false; }
    struct frame *frame = calloc(1, sizeof(*frame));
    if (!frame) { munmap(pixels, WIDTH * HEIGHT * 4); close(fd); fail(a, "allocation failed"); return false; }
    struct wl_shm_pool *pool = wl_shm_create_pool(a->shm, fd, WIDTH * HEIGHT * 4);
    frame->buffer = wl_shm_pool_create_buffer(pool, 0, WIDTH, HEIGHT, WIDTH * 4, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    close(fd);
    frame->app = a; frame->pixels = pixels; frame->next = a->frames;
    a->frames = frame; a->frame_count++;
    wl_buffer_add_listener(frame->buffer, &buffer_listener, frame);
    for (int y = 0; y < HEIGHT; ++y) for (int x = 0; x < WIDTH; ++x) {
        uint32_t color = 0xff18232d;
        if (y >= 34 && y < 67) color = a->selected_row == 1 ? 0xff216477 : 0xff263746;
        if (y >= 70 && y < 103) color = a->selected_row == 2 ? 0xff216477 : 0xff263746;
        if (x < 3 || y < 3 || x >= WIDTH - 3 || y >= HEIGHT - 3) color = 0xffffcf40;
        pixels[y * WIDTH + x] = color;
    }
    label(pixels, 12, 10, "IME 216", 0xfff5f5f5);
    label(pixels, 12, 43, "1 NI", 0xfff5f5f5);
    label(pixels, 12, 79, "2 HON", 0xfff5f5f5);
    wl_surface_attach(a->surface, frame->buffer, 0, 0);
    wl_surface_damage(a->surface, 0, 0, WIDTH, HEIGHT);
    wl_surface_commit(a->surface);
    prefix(a, "popup_frame");
    printf(",\"width\":%d,\"height\":%d,\"selected_row\":%u}\n", WIDTH, HEIGHT, a->selected_row);
    return true;
}

static void command(struct app *a, char *line) {
    char *argument = strchr(line, ' ');
    if (argument) *argument++ = 0;
    else argument = line + strlen(line);
    a->sequence++;
    if (!valid_utf8((unsigned char *)argument) || strlen(argument) > MAX_TEXT) {
        fail(a, "invalid UTF-8 or oversized command payload"); return;
    }
    /* Fetch already queued compositor state before selecting the commit serial. */
    if (wl_display_roundtrip(a->display) < 0) { fail(a, "Wayland roundtrip failed"); return; }
    if (!a->running) return;
    if (!strcmp(line, "quit") && !*argument) { event(a, "quit", ""); a->running = false; return; }
    if (!strcmp(line, "status") && !*argument) { event(a, "status", ""); return; }
    if (!strcmp(line, "row") && (!strcmp(argument, "1") || !strcmp(argument, "2"))) {
        a->selected_row = (unsigned)(argument[0] - '0');
        render_popup(a);
        return;
    }
    if (strcmp(line, "preedit") && strcmp(line, "commit") && strcmp(line, "cancel")) {
        fail(a, "unknown command; expected preedit, commit, cancel, row, status, quit"); return;
    }
    if (!strcmp(line, "cancel") && *argument) { fail(a, "cancel takes no payload"); return; }
    if (!a->active) { fail(a, "composition command while inactive"); return; }
    if (!strcmp(line, "preedit")) {
        int32_t end = (int32_t)strlen(argument);
        zwp_input_method_v2_set_preedit_string(a->ime, argument, end, end);
    } else {
        zwp_input_method_v2_set_preedit_string(a->ime, "", 0, 0);
        if (!strcmp(line, "commit")) zwp_input_method_v2_commit_string(a->ime, argument);
    }
    prefix(a, "request"); printf(",\"operation\":"); json_string(line);
    printf(",\"text\":"); json_string(argument);
    printf(",\"bytes\":%zu}\n", strlen(argument));
    zwp_input_method_v2_commit(a->ime, a->serial);
    if (wl_display_roundtrip(a->display) < 0) fail(a, "Wayland command roundtrip failed");
    else event(a, "request_processed_by_compositor", "not an application acknowledgement");
}
static void cleanup(struct app *a) {
    if (a->popup) zwp_input_popup_surface_v2_destroy(a->popup);
    if (a->surface) wl_surface_destroy(a->surface);
    while (a->frames) free_frame(a->frames);
    if (a->ime) zwp_input_method_v2_destroy(a->ime);
    if (a->manager) zwp_input_method_manager_v2_destroy(a->manager);
    if (a->seat) wl_seat_destroy(a->seat);
    if (a->shm) wl_shm_destroy(a->shm);
    if (a->compositor) wl_compositor_destroy(a->compositor);
    if (a->registry) wl_registry_destroy(a->registry);
    if (a->display) { wl_display_flush(a->display); wl_display_disconnect(a->display); }
}
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--help")) {
        puts("Usage: ime216 RUNTIME_DIR RIG_SOCKET TIMEOUT_SECONDS\n"
             "Explicit isolated rig socket required. Commands on stdin:\n"
             "preedit TEXT | commit TEXT | cancel | row 1 | row 2 | status | quit\n"
             "Runtime timeout: 1..1800 seconds. No keyboard grab or production engine.");
        return 0;
    }
    if (argc != 4 || argv[1][0] != '/' || !*argv[2] || strchr(argv[2], '/')) {
        fputs("ime216: require absolute runtime directory, explicit socket basename, timeout\n", stderr);
        return 2;
    }
    char *end = NULL;
    errno = 0;
    long timeout = strtol(argv[3], &end, 10);
    if (errno || !end || *end || timeout < 1 || timeout > 1800) {
        fputs("ime216: timeout must be 1..1800 seconds\n", stderr); return 2;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);
    signal(SIGALRM, expired);
    signal(SIGPIPE, SIG_IGN);
    alarm((unsigned)timeout);
    /* libwayland otherwise prefers this inherited FD over the explicit name. */
    if (unsetenv("WAYLAND_SOCKET") < 0) {
        fputs("ime216: cannot clear inherited WAYLAND_SOCKET override\n", stderr);
        return 1;
    }
    if (setenv("XDG_RUNTIME_DIR", argv[1], 1) < 0) return 1;
    struct app a = {.running = true, .selected_row = 1};
    event(&a, "connect", argv[2]);
    a.display = wl_display_connect(argv[2]);
    if (!a.display) { fail(&a, "cannot connect to explicit rig socket"); return 1; }
    a.registry = wl_display_get_registry(a.display);
    wl_registry_add_listener(a.registry, &registry_listener, &a);
    if (wl_display_roundtrip(a.display) < 0) fail(&a, "registry roundtrip failed");
    if (a.running && (!a.compositor || !a.shm || !a.seat || !a.manager ||
                      a.seat_count != 1 || a.manager_count != 1 || !a.text_input_v3))
        fail(&a, "missing or ambiguous required globals");
    if (a.running) {
        a.ime = zwp_input_method_manager_v2_get_input_method(a.manager, a.seat);
        zwp_input_method_v2_add_listener(a.ime, &ime_listener, &a);
        if (wl_display_roundtrip(a.display) < 0) fail(&a, "input method roundtrip failed");
    }
    if (a.running) {
        a.surface = wl_compositor_create_surface(a.compositor);
        a.popup = zwp_input_method_v2_get_input_popup_surface(a.ime, a.surface);
        zwp_input_popup_surface_v2_add_listener(a.popup, &popup_listener, &a);
        render_popup(&a);
        event(&a, "ready", "await activate/done before composition commands");
    }
    char line[MAX_LINE];
    size_t used = 0;
    while (a.running) {
        bool prepared = false;
        while (a.running) {
            if (wl_display_prepare_read(a.display) == 0) { prepared = true; break; }
            if (wl_display_dispatch_pending(a.display) < 0) fail(&a, "dispatch failed");
        }
        if (!a.running) {
            if (prepared) wl_display_cancel_read(a.display);
            break;
        }
        short display_events = POLLIN;
        if (wl_display_flush(a.display) < 0) {
            if (errno == EAGAIN) display_events |= POLLOUT;
            else { wl_display_cancel_read(a.display); fail(&a, "flush failed"); break; }
        }
        struct pollfd fds[2] = {{wl_display_get_fd(a.display), display_events, 0}, {STDIN_FILENO, POLLIN, 0}};
        int count = poll(fds, 2, -1);
        if (count < 0) {
            wl_display_cancel_read(a.display);
            if (errno == EINTR) continue;
            fail(&a, "poll failed"); break;
        }
        if (fds[0].revents & POLLIN) {
            if (wl_display_read_events(a.display) < 0) { fail(&a, "display disconnected"); break; }
            if (wl_display_dispatch_pending(a.display) < 0) { fail(&a, "event dispatch failed"); break; }
        } else wl_display_cancel_read(a.display);
        if (fds[0].revents & (POLLERR | POLLHUP | POLLNVAL)) { fail(&a, "display socket closed"); break; }
        if (!a.running) break;
        if (fds[1].revents & (POLLIN | POLLHUP)) {
            unsigned char chunk[512];
            ssize_t bytes = read(STDIN_FILENO, chunk, sizeof(chunk));
            if (bytes < 0) { if (errno == EINTR) continue; fail(&a, "stdin read failed"); break; }
            if (!bytes) {
                if (used) fail(&a, "incomplete command at stdin EOF");
                else event(&a, "eof", "");
                break;
            }
            for (ssize_t i = 0; i < bytes && a.running; ++i) {
                if (!chunk[i]) { fail(&a, "NUL in command"); break; }
                if (chunk[i] == '\n') {
                    if (used && line[used - 1] == '\r') --used;
                    line[used] = 0;
                    command(&a, line);
                    used = 0;
                } else if (used + 1 < sizeof(line)) line[used++] = (char)chunk[i];
                else { fail(&a, "command line too long"); break; }
            }
        }
        if (fds[1].revents & (POLLERR | POLLNVAL)) { fail(&a, "stdin unavailable"); break; }
    }
    event(&a, "shutdown", a.result ? "failed" : "normal");
    cleanup(&a);
    return a.result;
}
