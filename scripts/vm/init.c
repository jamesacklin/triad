// Minimal PID 1 for the triad guest. It mounts /proc, /sys and /tmp, mounts
// a second disk on /data if there is one, copies the DHCP nameserver into
// /etc/resolv.conf and sets the KEY=VALUE lines of /etc/triad.env in its
// environment. Then it does one of two things.
//
// Test run (the default): runs
// `triad --data-dir /data <args from /etc/triad.args> --json` with a pipe on
// stdin, sends one chat line (/etc/triad.line, if present), prints memory
// figures, sends /quit and reboots.
//
// Supervisor (TRIAD_SUPERVISE=1 in /etc/triad.env): keeps triad running and
// updated. triad is started from the A/B slot named by /data/bin/current, or
// from /bin/triad when there is none. Every so often that same binary is run
// as `triad update --slots /data/bin`; when it exits 10 it has installed a
// release into the other slot and switched `current`, and triad is
// restarted. A new binary is on trial while /data/bin/pending exists: if
// triad stays up TRIAD_CONFIRM_SECS the file is deleted, and if triad exits
// first, `current` is set back to what `pending` names and `pending` is
// renamed `rolled-back`, which tells the updater not to install that release
// again. The format of these files is described in crates/triad-update.
// This file has no network or crypto code: the updater checks the release.
//
//   TRIAD_UPDATE_FIRST     seconds after boot before the first check (30)
//   TRIAD_UPDATE_INTERVAL  seconds between checks (21600)
//   TRIAD_CONFIRM_SECS     seconds a new binary must stay up (30)
//   TRIAD_LIFETIME         seconds before shutting down; 0 for never (0)
//
// The first triad of a boot gets the arguments in /etc/triad.args, later
// ones those in /etc/triad.resume (default `chat`), so that a restart
// reopens the channels instead of creating or joining again.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mount.h>
#include <sys/wait.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <signal.h>
#include <time.h>

static void grep_file(const char *path, const char *keys[]) {
    FILE *f = fopen(path, "r"); char line[512];
    if (!f) { printf("cannot open %s\n", path); return; }
    while (fgets(line, sizeof line, f))
        for (int i = 0; keys[i]; i++)
            if (!strncmp(line, keys[i], strlen(keys[i]))) fputs(line, stdout);
    fclose(f);
}

static void print_mem(pid_t pid) {
    puts("---MEM---");
    const char *mk[] = {"MemTotal", "MemFree", "MemAvailable", "Cached", "AnonPages", "Slab", NULL};
    grep_file("/proc/meminfo", mk);
    char path[64]; snprintf(path, sizeof path, "/proc/%d/status", pid);
    const char *sk[] = {"VmRSS", "VmHWM", "RssAnon", "RssFile", "Threads", NULL};
    grep_file(path, sk);
    puts("---END---");
}

// Fills argv with `<bin> --data-dir /data <words of file, or of dflt> --json`.
static void triad_argv(char *argv[32], const char *bin, const char *file, const char *dflt) {
    static char args[4096];
    int n = 0;
    argv[n++] = (char *)bin; argv[n++] = "--data-dir"; argv[n++] = "/data";
    args[0] = 0;
    FILE *a = fopen(file, "r");
    if (!a || !fgets(args, sizeof args, a) || !args[strspn(args, " \n")]) snprintf(args, sizeof args, "%s", dflt);
    if (a) fclose(a);
    for (char *t = strtok(args, " \n"); t && n < 29; t = strtok(NULL, " \n")) argv[n++] = t;
    argv[n++] = "--json"; argv[n] = NULL;
}

#define SLOTS "/data/bin"

static double now(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int env_int(const char *key, int dflt) {
    const char *v = getenv(key);
    return v && *v ? atoi(v) : dflt;
}

// The first word of a small file, or "" if it cannot be read.
static void read_word(const char *path, char *word, size_t size) {
    word[0] = 0;
    FILE *f = fopen(path, "r");
    if (!f) return;
    if (!fgets(word, size, f)) word[0] = 0;
    fclose(f);
    word[strcspn(word, " \n")] = 0;
}

// The binary to run and the name of its slot: the one `current` names if it
// is there and executable, else the one in the image.
static const char *pick(char slot[16]) {
    static char path[64];
    read_word(SLOTS "/current", slot, 16);
    if (!strcmp(slot, "a") || !strcmp(slot, "b")) {
        snprintf(path, sizeof path, SLOTS "/%s", slot);
        if (!access(path, X_OK)) return path;
    }
    strcpy(slot, "factory");
    return "/bin/triad";
}

// A failed trial: point `current` back at what ran before and leave
// `rolled-back` for the updater.
static void roll_back(const char *from) {
    char prev[16];
    read_word(SLOTS "/pending", prev, sizeof prev);
    if (!strcmp(prev, "a") || !strcmp(prev, "b")) {
        FILE *f = fopen(SLOTS "/current.tmp", "w");
        if (f) { fputs(prev, f); fflush(f); fsync(fileno(f)); fclose(f); rename(SLOTS "/current.tmp", SLOTS "/current"); }
    } else {
        strcpy(prev, "factory");
        unlink(SLOTS "/current");
    }
    rename(SLOTS "/pending", SLOTS "/rolled-back");
    sync();
    printf("SUPERVISOR rollback from=%s to=%s\n", from, prev);
}

static int supervise(void) {
    int first_s = env_int("TRIAD_UPDATE_FIRST", 30), interval_s = env_int("TRIAD_UPDATE_INTERVAL", 21600);
    int confirm_s = env_int("TRIAD_CONFIRM_SECS", 30), lifetime_s = env_int("TRIAD_LIFETIME", 0);
    double boot = now(), next_check = boot + first_s;
    pid_t updater = 0;
    int first = 1, shutting = 0;

    while (!shutting) {
        char slot[16];
        const char *bin = pick(slot);
        int trial = !access(SLOTS "/pending", F_OK);
        printf("SUPERVISOR start slot=%s trial=%d\n", slot, trial);
        pid_t v = fork();
        if (v == 0) { execl(bin, bin, "--version", (char *)NULL); _exit(127); }
        if (v > 0) waitpid(v, NULL, 0);

        char *argv[32];
        triad_argv(argv, bin, first ? "/etc/triad.args" : "/etc/triad.resume", "chat");
        first = 0;
        int p[2];
        if (pipe(p)) { perror("pipe"); return 1; }
        pid_t pid = fork();
        if (pid == 0) { dup2(p[0], 0); close(p[0]); close(p[1]); execv(argv[0], argv); perror("execv"); _exit(127); }
        close(p[0]);

        double started = now(), quit_at = 0;
        int restart = 0, st = 0;
        for (;;) {
            sleep(1);
            int exited = 0, s; pid_t r;
            while ((r = waitpid(-1, &s, WNOHANG)) > 0) {
                if (r == pid) { exited = 1; st = s; }
                else if (r == updater) {
                    int code = WIFEXITED(s) ? WEXITSTATUS(s) : 128 + WTERMSIG(s);
                    printf("SUPERVISOR updater exit=%d\n", code);
                    updater = 0; next_check = now() + interval_s;
                    if (code == 10) restart = 1;
                }
            }
            if (exited) break;
            double t = now();
            if (trial && t - started >= confirm_s) {
                unlink(SLOTS "/pending"); sync(); trial = 0;
                printf("SUPERVISOR confirmed slot=%s\n", slot);
            }
            if (lifetime_s && t - boot >= lifetime_s && !shutting) { shutting = 1; print_mem(pid); }
            if ((restart || shutting) && !quit_at) {
                if (restart) printf("SUPERVISOR restarting triad for the update\n");
                dprintf(p[1], "/quit\n"); quit_at = t;
            }
            if (quit_at && t - quit_at > 15) kill(pid, SIGKILL);
            if (!updater && !trial && !quit_at && t >= next_check) {
                updater = fork();
                if (updater == 0) { execl(bin, bin, "update", "--slots", SLOTS, (char *)NULL); perror("execl"); _exit(127); }
            }
        }
        close(p[1]);
        printf("SUPERVISOR triad exit=%d slot=%s\n", WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st), slot);
        if (shutting) break;
        // Nobody asked a binary on trial to stop, so it failed.
        if (trial) roll_back(slot);
        else if (!restart) { printf("SUPERVISOR triad stopped by itself; starting it again\n"); sleep(5); }
    }
    sync();
    reboot(RB_AUTOBOOT);
    for (;;) pause();
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("sys", "/sys", "sysfs", 0, NULL);
    mount("tmp", "/tmp", "tmpfs", 0, NULL);
    // A second disk, if the VM has one, holds /data across boots.
    if (!access("/sys/block/vdb", F_OK) && mount("/dev/vdb", "/data", "ext4", 0, NULL)) perror("mount /dev/vdb on /data");
    setenv("HOME", "/data", 1);

    char line[4096];
    FILE *in = fopen("/proc/net/pnp", "r"), *out = fopen("/etc/resolv.conf", "w");
    if (in && out) while (fgets(line, sizeof line, in)) if (!strncmp(line, "nameserver", 10)) fputs(line, out);
    if (in) fclose(in);
    if (out) fclose(out);

    FILE *e = fopen("/etc/triad.env", "r");
    static char env_lines[64][512]; int ne = 0;
    while (e && ne < 64 && fgets(env_lines[ne], sizeof env_lines[ne], e)) {
        char *l = env_lines[ne], *eq = strchr(l, '=');
        l[strcspn(l, "\n")] = 0;
        if (eq) { *eq = 0; setenv(l, eq + 1, 1); ne++; }
    }
    if (e) fclose(e);

    if (env_int("TRIAD_SUPERVISE", 0)) return supervise();

    char *argv[32];
    triad_argv(argv, "/bin/triad", "/etc/triad.args", "");
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
    // /etc/triad.line: the line to send (default "hello from the vm").
    static char send_line[1024] = "hello from the vm";
    FILE *sl = fopen("/etc/triad.line", "r");
    if (sl) { if (!fgets(send_line, sizeof send_line, sl)) strcpy(send_line, "hello from the vm"); fclose(sl); }
    send_line[strcspn(send_line, "\n")] = 0;
    if (!send_line[0]) strcpy(send_line, "hello from the vm");
    dprintf(p[1], "%s\n", send_line);
    sleep(settle_s);
    print_mem(pid);
    dprintf(p[1], "/quit\n");
    int st = 0; waitpid(pid, &st, 0);
    printf("TRIAD_EXIT=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st));
    sync();
    reboot(RB_AUTOBOOT);
    for (;;) pause();
}
