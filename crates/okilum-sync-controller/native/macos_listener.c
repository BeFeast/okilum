// Use the SDK's ABI definitions, never hand-maintained Rust socket_info layouts.
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>

int okilum_macos_owned_listener(int pid, int family,
                              const unsigned char *address, unsigned short port) {
    // A full result may have been truncated by concurrent descriptor creation.
    // Grow and retry; failure/uncertainty never becomes ownership evidence.
    for (size_t slots = 128; slots <= 65536; slots *= 2) {
        size_t capacity = slots * sizeof(struct proc_fdinfo);
        struct proc_fdinfo *fds = calloc(slots, sizeof(*fds));
        if (!fds) return -1;
        int count = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds, (int)capacity);
        if (count <= 0 || (size_t)count % sizeof(*fds) != 0) {
            free(fds);
            return -1;
        }
        if ((size_t)count >= capacity) {
            free(fds);
            continue;
        }
        int found = 0;
        for (size_t i = 0; i < (size_t)count / sizeof(*fds); ++i) {
            if (fds[i].proc_fdtype != PROX_FDTYPE_SOCKET) continue;
            struct socket_fdinfo socket;
            int read = proc_pidfdinfo(pid, fds[i].proc_fd, PROC_PIDFDSOCKETINFO,
                                      &socket, sizeof(socket));
            // Ownership is existential: an unrelated FD closing cannot
            // invalidate a successfully inspected matching listener.
            if (read != (int)sizeof(socket)) continue;
            const struct socket_info *info = &socket.psi;
            if (info->soi_kind != SOCKINFO_TCP || info->soi_family != family ||
                info->soi_proto.pri_tcp.tcpsi_state != TSI_S_LISTEN) continue;
            const struct in_sockinfo *ip = &info->soi_proto.pri_tcp.tcpsi_ini;
            if (ntohs((unsigned short)ip->insi_lport) != port) continue;
            if ((family == AF_INET &&
                 memcmp(&ip->insi_laddr.ina_46.i46a_addr4, address, 4) == 0) ||
                (family == AF_INET6 &&
                 memcmp(&ip->insi_laddr.ina_6, address, 16) == 0)) found = 1;
        }
        free(fds);
        return found;
    }
    return -1;
}
