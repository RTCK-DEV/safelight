/* SPDX-License-Identifier: MIT */
/* Decode RAW sensor data via macOS ImageIO (system RAW codecs).
   Used as a fallback when libraw can parse the container but cannot
   decompress the sensor data (e.g. Nikon HE / HE* "TicoRAW", which the
   system codec decodes at full resolution). */
#include "ara_shim.h"

#if defined(__APPLE__)

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>

extern "C" int ara_imgio_decode(const char* path, unsigned short** out,
                                int* w, int* h) {
  @autoreleasepool {
    if (!path || !out || !w || !h) return -1;
    NSString* ns = [NSString stringWithUTF8String:path];
    if (!ns) return -2;
    NSURL* url = [NSURL fileURLWithPath:ns];
    CGImageSourceRef src = CGImageSourceCreateWithURL(
        (__bridge CFURLRef)url, nullptr);
    if (!src) return -3;
    NSDictionary* opts = @{
      (NSString*)kCGImageSourceShouldAllowFloat : @YES,
      (NSString*)kCGImageSourceShouldCache : @NO,
    };
    CGImageRef img = CGImageSourceCreateImageAtIndex(
        src, 0, (__bridge CFDictionaryRef)opts);
    CFRelease(src);
    if (!img) return -4;
    size_t iw = CGImageGetWidth(img);
    size_t ih = CGImageGetHeight(img);
    if (iw < 8 || ih < 8 || iw > 30000 || ih > 30000) {
      CGImageRelease(img);
      return -5;
    }
    /* RGBA16 little-endian, non-premultiplied target */
    size_t rowbytes = iw * 8;
    unsigned short* buf = (unsigned short*)malloc(rowbytes * ih);
    if (!buf) {
      CGImageRelease(img);
      return -6;
    }
    CGColorSpaceRef cs = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef ctx = CGBitmapContextCreate(
        buf, iw, ih, 16, rowbytes, cs,
        kCGImageAlphaNoneSkipLast | kCGBitmapByteOrder16Little);
    if (!ctx) {
      free(buf);
      CGColorSpaceRelease(cs);
      CGImageRelease(img);
      return -7;
    }
    CGContextDrawImage(ctx, CGRectMake(0, 0, iw, ih), img);
    CGContextRelease(ctx);
    CGColorSpaceRelease(cs);
    CGImageRelease(img);
    *out = buf;
    *w = (int)iw;
    *h = (int)ih;
    return 0;
  }
}

#else

extern "C" int ara_imgio_decode(const char*, unsigned short**, int*, int*) {
  return -1;
}

#endif
