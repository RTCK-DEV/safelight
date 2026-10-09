#ifndef SAFELIGHT_H
#define SAFELIGHT_H

#include <stdint.h>

typedef struct {
    uint8_t* data;
    uintptr_t len;
    uint32_t width;
    uint32_t height;
} SlImage;

/// 256-bin x 4-channel (R,G,B,luma) histogram of a rendered image.
typedef struct {
    uint32_t bins[1024];
} SlHistogram;



void* safelight_init(void);
void safelight_free_engine(void* e);
char* safelight_last_error(void);
void safelight_free_string(char* s);
void safelight_free_image(SlImage img);

char* safelight_scan_folder(void* e, const char* folder);
SlImage safelight_thumbnail(void* e, const char* path, uint32_t max_px);
SlImage safelight_render(void* e, const char* path, const char* recipe_json, uint32_t max_px);
SlImage safelight_render_h(void* e, const char* path, const char* recipe_json, uint32_t max_px, SlHistogram* hist);
SlImage safelight_scopes(void* e, const char* path, const char* recipe_json, uint32_t max_px, uint32_t* wave, uint32_t* vec, uint32_t* cie, uint32_t* hist);
SlImage safelight_export(void* e, const char* path, const char* recipe_json);
SlImage safelight_export_opts(void* e, const char* path, const char* recipe_json, const char* opts_json);
SlImage safelight_merge(void* e, const char* paths_json, const char* mode);
char* safelight_metadata(void* e, const char* path);
char* safelight_sidecar_read(const char* path);
int safelight_sidecar_write(const char* path, const char* json);
char* safelight_sidecar_read_v(const char* path, int vslot);
int safelight_sidecar_write_v(const char* path, int vslot, const char* json);
char* safelight_library(void* e, const char* cmd_json);
int safelight_set_rating(void* e, const char* path, int rating);
int safelight_set_label(void* e, const char* path, const char* label);
SlImage safelight_reference(const char* path);

char* safelight_auto_analyze(void* e, const char* path);
char* safelight_ai_denoise_prepare(void* e, const char* path, const char* recipe_json);
char* safelight_ai_denoise_ready(void* e, const char* path);
char* safelight_ai_subject_prepare(void* e, const char* path);
char* safelight_ai_subject_ready(void* e, const char* path);

#endif
