#ifndef ARAWARE_H
#define ARAWARE_H

#include <stdint.h>

typedef struct {
    uint8_t* data;
    uintptr_t len;
    uint32_t width;
    uint32_t height;
} AraImage;

/// 256-bin x 4-channel (R,G,B,luma) histogram of a rendered image.
typedef struct {
    uint32_t bins[1024];
} AraHistogram;



void* araware_init(void);
void araware_free_engine(void* e);
char* araware_last_error(void);
void araware_free_string(char* s);
void araware_free_image(AraImage img);

char* araware_scan_folder(void* e, const char* folder);
AraImage araware_thumbnail(void* e, const char* path, uint32_t max_px);
AraImage araware_render(void* e, const char* path, const char* recipe_json, uint32_t max_px);
AraImage araware_render_h(void* e, const char* path, const char* recipe_json, uint32_t max_px, AraHistogram* hist);
AraImage araware_scopes(void* e, const char* path, const char* recipe_json, uint32_t max_px, uint32_t* wave, uint32_t* vec, uint32_t* cie, uint32_t* hist);
AraImage araware_export(void* e, const char* path, const char* recipe_json);
char* araware_metadata(void* e, const char* path);
char* araware_sidecar_read(const char* path);
int araware_sidecar_write(const char* path, const char* json);
int araware_set_rating(void* e, const char* path, int rating);
int araware_set_label(void* e, const char* path, const char* label);
AraImage araware_reference(const char* path);

#endif
