/* Minimal Mupen64Plus front end: dummy video/audio, rsp-hle, the m64 input
 * plugin (m64_input.so). Usage: frontend ROM RSP_SO INPUT_SO VIDEO_SO [CORE_SO] */
#include <stdio.h>
#include <stdlib.h>
#include <dlfcn.h>
#include "m64p_types.h"
#include "m64p_frontend.h"
static void dbg(void *c, int l, const char *m) { if (l <= 2) fprintf(stderr, "[m64p] %s\n", m); }
static void st(void *c, m64p_core_param p, int v) {}
#define NEED(h, n) ((void)0)
int main(int argc, char **argv) {
  if (argc < 5) { fprintf(stderr, "frontend ROM RSP_SO INPUT_SO VIDEO_SO [CORE_SO]\n"); return 2; }
  void *core = dlopen(argc > 5 ? argv[5] : "libmupen64plus.so.2", RTLD_NOW | RTLD_GLOBAL);
  if (!core) { fprintf(stderr, "core: %s\n", dlerror()); return 1; }
  m64p_error (*Startup)(int, const char *, const char *, void *, ptr_DebugCallback, void *, ptr_StateCallback) = dlsym(core, "CoreStartup");
  m64p_error (*Do)(m64p_command, int, void *) = dlsym(core, "CoreDoCommand");
  m64p_error (*Attach)(m64p_plugin_type, m64p_dynlib_handle) = dlsym(core, "CoreAttachPlugin");
  m64p_error (*Config)(const char*, m64p_handle*) = dlsym(core, "ConfigOpenSection");
  if (Startup(0x020001, getenv("HOME") ? getenv("HOME") : "/tmp", "/usr/share/mupen64plus", NULL, dbg, NULL, st)) return 1;
  FILE *f = fopen(argv[1], "rb"); fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
  void *rom = malloc(n); if (fread(rom, 1, n, f) != (size_t)n) return 1; fclose(f);
  if (Do(M64CMD_ROM_OPEN, (int)n, rom)) { fprintf(stderr, "rom open failed\n"); return 1; }
  /* the core wants GFX, AUDIO, INPUT, RSP in this order (AUDIO stays the built-in dummy) */
  const char *plug[3] = {argv[4], argv[3], argv[2]}; m64p_plugin_type ty[3] = {M64PLUGIN_GFX, M64PLUGIN_INPUT, M64PLUGIN_RSP};
  for (int i = 0; i < 3; i++) {
    void *h = dlopen(plug[i], RTLD_NOW); if (!h) { fprintf(stderr, "%s: %s\n", plug[i], dlerror()); return 1; }
    m64p_error (*ps)(m64p_dynlib_handle, void *, void *) = dlsym(h, "PluginStartup");
    if (ps(core, NULL, dbg)) { fprintf(stderr, "PluginStartup %s failed\n", plug[i]); return 1; }
    if (Attach(ty[i], h)) { fprintf(stderr, "attach %s failed\n", plug[i]); return 1; } }
  int off = 0; Do(M64CMD_CORE_STATE_SET, M64CORE_SPEED_LIMITER, &off);
  int r = Do(M64CMD_EXECUTE, 0, NULL);
  fprintf(stderr, "execute returned %d\n", r);
  Do(M64CMD_ROM_CLOSE, 0, NULL);
  return 0; }
