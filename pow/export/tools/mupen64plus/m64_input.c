/* Mupen64Plus input plugin that plays an .m64 file (4-byte frames: the file's
 * little-endian u32 IS Mupen64Plus' BUTTONS.Value) and asks the core to save
 * a state at chosen polls. Env: M64_FILE (movie), M64_SAVE_AT (comma separated
 * poll numbers, 1-based: "after N polls"), M64_SAVE_DIR (state files), and
 * M64_QUIT_AT (poll after which to stop the emulator). Test tool, not a product. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#define M64P_PLUGIN_PROTOTYPES 1
#include "m64p_types.h"
#include "m64p_plugin.h"

static unsigned char *movie; static long nframes; static long poll;
static int (*core_do)(m64p_command, int, void *);
static long saves[4096]; static int nsaves;
static long quit_at = -1;
static const char *save_dir = ".";
/* M64_DUMP="a-b" and M64_DUMP_FILE: append, after every poll a..b (1-based "after N polls"), the RDRAM ranges
 * a diff tool wants (Mario, RNG, timer, the 240-object pool) as raw big-endian bytes, without save states
 * (the core only completes the first few state saves of a run). Needs a core with the debugger interface. */
static unsigned char *(*dbg_ptr)(int); static long dump_lo = -1, dump_hi = -1; static FILE *dump_f;
static const struct { unsigned addr, len; } dump_rng[] = { {0x8033B170, 0xC8}, {0x8038EEE0, 4}, {0x8032D5D4, 4}, {0x8033D488, 240 * 0x260} };

EXPORT m64p_error CALL PluginGetVersion(m64p_plugin_type *t, int *v, int *a, const char **n, int *c) {
  if (t) *t = M64PLUGIN_INPUT; if (v) *v = 0x020000; if (a) *a = 0x020100;
  if (n) *n = "sm64coin m64 input"; if (c) *c = 0; return M64ERR_SUCCESS; }
EXPORT m64p_error CALL PluginStartup(m64p_dynlib_handle core, void *ctx, void (*dbg)(void *, int, const char *)) {
  core_do = (int (*)(m64p_command, int, void *))dlsym(core, "CoreDoCommand");
  const char *f = getenv("M64_FILE"); FILE *fp = f ? fopen(f, "rb") : NULL;
  if (!fp) { fprintf(stderr, "M64_FILE?\n"); return M64ERR_INPUT_NOT_FOUND; }
  fseek(fp, 0, SEEK_END); long n = ftell(fp); fseek(fp, 0x400, SEEK_SET);
  nframes = (n - 0x400) / 4; movie = malloc(nframes * 4 + 4);
  if (fread(movie, 4, nframes, fp) != (size_t)nframes) return M64ERR_INPUT_NOT_FOUND; fclose(fp);
  char *s = getenv("M64_SAVE_AT"); if (s) { s = strdup(s); for (char *t = strtok(s, ","); t; t = strtok(NULL, ",")) saves[nsaves++] = atol(t); }
  if (getenv("M64_QUIT_AT")) quit_at = atol(getenv("M64_QUIT_AT"));
  if (getenv("M64_DUMP") && getenv("M64_DUMP_FILE")) {
    sscanf(getenv("M64_DUMP"), "%ld-%ld", &dump_lo, &dump_hi); dump_f = fopen(getenv("M64_DUMP_FILE"), "wb");
    dbg_ptr = (unsigned char *(*)(int))dlsym(core, "DebugMemGetPointer"); }
  if (getenv("M64_SAVE_DIR")) save_dir = getenv("M64_SAVE_DIR");
  return M64ERR_SUCCESS; }
EXPORT m64p_error CALL PluginShutdown(void) { return M64ERR_SUCCESS; }
EXPORT void CALL InitiateControllers(CONTROL_INFO ci) {
  for (int i = 0; i < 4; i++) { ci.Controls[i].Present = i == 0; ci.Controls[i].RawData = 0; ci.Controls[i].Plugin = PLUGIN_NONE; } }
EXPORT void CALL GetKeys(int c, BUTTONS *k) {
  k->Value = 0; if (c != 0) return;
  if (poll == 0) { int off = 0; int r = core_do(M64CMD_CORE_STATE_SET, M64CORE_SPEED_LIMITER, &off); (void)r; }
  /* poll number `poll` (0-based) gets frame poll */
  if (poll < nframes) memcpy(&k->Value, movie + poll * 4, 4);
  poll++;
    for (int i = 0; i < nsaves; i++) if (saves[i] == poll) {
    char p[1024]; snprintf(p, sizeof p, "%s/after_%ld.st", save_dir, poll);
    core_do(M64CMD_STATE_SAVE, 1, p); }
  if (dump_f && poll >= dump_lo && poll <= dump_hi) {
    unsigned char *r = dbg_ptr ? dbg_ptr(1 /* M64P_DBG_PTR_RDRAM */) : NULL;
    if (!r) { fprintf(stderr, "no RDRAM pointer (core without debugger)\n"); dump_f = NULL; }
    else { long hdr[2] = { poll, 0 }; unsigned nr = getenv("M64_DUMP_NOPOOL") ? 3 : 4; fwrite(hdr, sizeof hdr, 1, dump_f);
      for (unsigned i = 0; i < nr; i++) {
        /* RDRAM is host-order words: the byte at N64 address A is r[(A & 0x7FFFFF) ^ 3] */
        for (unsigned k = 0; k < dump_rng[i].len; k++) fputc(r[((dump_rng[i].addr + k) & 0x7FFFFF) ^ 3], dump_f); } fflush(dump_f); } }
  if (quit_at >= 0 && poll > quit_at) core_do(M64CMD_STOP, 0, NULL); }
EXPORT void CALL ControllerCommand(int c, unsigned char *cmd) {}
EXPORT void CALL ReadController(int c, unsigned char *cmd) {}
EXPORT void CALL SDL_KeyDown(int m, int k) {}
EXPORT void CALL SDL_KeyUp(int m, int k) {}
EXPORT int CALL RomOpen(void) { return 1; }
EXPORT void CALL RomClosed(void) {}
