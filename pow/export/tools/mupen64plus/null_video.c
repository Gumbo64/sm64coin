/* A video plugin that draws nothing but completes display lists: it raises the
 * RDP's "done" interrupt (MI_INTR_DP), which the game waits for. Mupen64Plus'
 * built-in dummy video does not, so the game would stall without this. */
#include <stdlib.h>
#include "m64p_types.h"
#include "m64p_plugin.h"
static GFX_INFO g;
EXPORT m64p_error CALL PluginGetVersion(m64p_plugin_type *t, int *v, int *a, const char **n, int *c) {
  if (t) *t = M64PLUGIN_GFX; if (v) *v = 0x010000; if (a) *a = 0x020100;
  if (n) *n = "sm64coin null video"; if (c) *c = 0; return M64ERR_SUCCESS; }
EXPORT m64p_error CALL PluginStartup(m64p_dynlib_handle core, void *ctx, void (*dbg)(void *, int, const char *)) { return M64ERR_SUCCESS; }
EXPORT m64p_error CALL PluginShutdown(void) { return M64ERR_SUCCESS; }
EXPORT int CALL InitiateGFX(GFX_INFO info) { g = info; return 1; }
EXPORT void CALL ProcessDList(void) { *g.MI_INTR_REG |= 0x20; g.CheckInterrupts(); }
EXPORT void CALL ProcessRDPList(void) { *g.MI_INTR_REG |= 0x20; g.CheckInterrupts(); }
EXPORT void CALL ChangeWindow(void) {}
EXPORT void CALL MoveScreen(int x, int y) {}
EXPORT void CALL ShowCFB(void) {}
EXPORT void CALL UpdateScreen(void) {}
EXPORT void CALL ViStatusChanged(void) {}
EXPORT void CALL ViWidthChanged(void) {}
EXPORT void CALL ReadScreen2(void *dest, int *width, int *height, int front) {}
EXPORT void CALL SetRenderingCallback(void (*cb)(int)) {}
EXPORT void CALL ResizeVideoOutput(int w, int h) {}
EXPORT void CALL FBWrite(unsigned int addr, unsigned int size) {}
EXPORT void CALL FBRead(unsigned int addr) {}
EXPORT void CALL FBGetFrameBufferInfo(void *p) {}
EXPORT int CALL RomOpen(void) { return 1; }
EXPORT void CALL RomClosed(void) {}
