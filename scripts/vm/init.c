// Minimal PID 1 for the triad test guest. It mounts /proc, /sys and /tmp,
// copies the DHCP nameserver into /etc/resolv.conf, runs
// `triad --data-dir /data <args from /etc/triad.args> --json` with a pipe on
// stdin, sends one chat line, prints memory figures, sends /quit and reboots.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mount.h>
#include <sys/wait.h>
#include <sys/reboot.h>

static void grep_file(const char *path, const char *keys[]) {
    FILE *f = fopen(path, "r"); char line[512];
    if (!f) { printf("cannot open %s\n", path); return; }
    while (fgets(line, sizeof line, f))
        for (int i = 0; keys[i]; i++)
            if (!strncmp(line, keys[i], strlen(keys[i]))) fputs(line, stdout);
    fclose(f);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("sys", "/sys", "sysfs", 0, NULL);
    mount("tmp", "/tmp", "tmpfs", 0, NULL);
    setenv("HOME", "/data", 1);

    char line[4096];
    FILE *in = fopen("/proc/net/pnp", "r"), *out = fopen("/etc/resolv.conf", "w");
    if (in && out) while (fgets(line, sizeof line, in)) if (!strncmp(line, "nameserver", 10)) fputs(line, out);
    if (in) fclose(in);
    if (out) fclose(out);

    char *argv[32]; int n = 0;
    argv[n++] = "/bin/triad"; argv[n++] = "--data-dir"; argv[n++] = "/data";
    static char args[4096];
    FILE *a = fopen("/etc/triad.args", "r");
    if (a && fgets(args, sizeof args, a))
        for (char *t = strtok(args, " \n"); t && n < 29; t = strtok(NULL, " \n")) argv[n++] = t;
    argv[n++] = "--json"; argv[n] = NULL;
    // /etc/triad.wait: seconds before sending, then seconds to let the reply arrive.
    int wait_s = 30, settle_s = 8;
    FILE *w = fopen("/etc/triad.wait", "r");
    if (w) { if (fscanf(w, "%d %d", &wait_s, &settle_s) < 1) wait_s = 30; fclose(w); }

    int p[2];
    if (pipe(p)) { perror("pipe"); return 1; }
    pid_t pid = fork();
    if (pid == 0) { dup2(p[0], 0); close(p[0]); close(p[1]); execv(argv[0], argv); perror("execv"); _exit(127); }
    close(p[0]);
    sleep(wait_s);
    dprintf(p[1], "hello from the vm\n");
    sleep(settle_s);
    puts("---MEM---");
    const char *mk[] = {"MemTotal", "MemFree", "MemAvailable", "Cached", "AnonPages", "Slab", NULL};
    grep_file("/proc/meminfo", mk);
    char path[64]; snprintf(path, sizeof path, "/proc/%d/status", pid);
    const char *sk[] = {"VmRSS", "VmHWM", "RssAnon", "RssFile", "Threads", NULL};
    grep_file(path, sk);
    puts("---END---");
    dprintf(p[1], "/quit\n");
    int st = 0; waitpid(pid, &st, 0);
    printf("TRIAD_EXIT=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st));
    sync();
    reboot(RB_AUTOBOOT);
    for (;;) pause();
}
