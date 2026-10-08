#ifndef ARA_SHIM_H
#define ARA_SHIM_H

/* SPDX-License-Identifier: MIT */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct AraRaw AraRaw;

typedef struct AraRawInfo {
  char make[64];
  char model[64];
  char lens[128];
  int raw_width;
  int raw_height;
  int width;
  int height;
  int top_margin;
  int left_margin;
  int flip;
  float black[4];
  unsigned int maximum;
  float cam_mul[4];
  float pre_mul[4];
  float cam_xyz[4][3];
  float rgb_cam[3][4];
  int cfa_kind;      /* 0 = already RGB / unknown, 1 = bayer-like via filters, 2 = xtrans */
  int cfa_pattern[36];
  int cfa_w;
  int cfa_h;
  int colors;        /* number of color channels in CFA (3 or 4) */
  int daylight_mul_valid;
  float iso;
  float shutter;
  float aperture;
  float focal;
  long timestamp;
} AraRawInfo;

AraRaw* ara_raw_open(const char* path, AraRawInfo* info);
int ara_raw_unpack(AraRaw* r);
/* re-read fields that libraw computes during unpack (black level,
   maximum, pre_mul, rgb_cam). Call after ara_raw_unpack(). */
void ara_raw_refresh_info(AraRaw* r, AraRawInfo* info);
/* returns malloc'ed copy of raw sensor data (cfa mosaic); free with ara_free */
int ara_raw_cfa(AraRaw* r, unsigned short** out, int* count);
/* returns malloc'ed thumbnail bytes; format: 1=jpeg,2=bitmap8,3=bitmap16 */
int ara_thumb(AraRaw* r, unsigned char** out, int* len, int* w, int* h, int* format);
/* renders with libraw's own pipeline -> malloc'ed rgb8 buffer (reference path) */
int ara_process8(AraRaw* r, unsigned char** out, int* w, int* h);
/* largest embedded thumbnail -> malloc'ed bytes; format: 1=jpeg,2=bitmap8,3=bitmap16 */
int ara_thumb_best(AraRaw* r, unsigned char** out, int* len, int* w, int* h,
                   int* format);
/* macOS ImageIO decode (full-res via system RAW codecs) -> malloc'ed rgba16 */
int ara_imgio_decode(const char* path, unsigned short** out, int* w, int* h);
/* GoPro GPR (VC-5 DNG) -> uncompressed DNG file at out_path. 0 = ok */
int ara_gpr_to_dng(const char* in_path, const char* out_path);
void ara_raw_close(AraRaw* r);
void ara_free(void* p);

#ifdef __cplusplus
}
#endif

#endif
