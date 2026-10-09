// GoPro GPR → DNG bridge via the vendored GPR SDK (source/lib/gpr_sdk,
// MIT OR Apache-2.0, © GoPro, Inc. — see third_party/gpr/LICENSE-*).
// VC-5-compressed DNG variants (HERO5..12) have no embedded JPEG and
// libraw needs the proprietary GoPro SDK for them; we decode here into
// an uncompressed DNG that libraw handles normally.

#include "sl_shim.h"

#include <cstdlib>

#include "gpr.h"
#include "gpr_buffer.h"

extern "C" int sl_gpr_to_dng(const char *in_path, const char *out_path) {
    if (!in_path || !out_path)
        return -1;

    gpr_allocator allocator;
    allocator.Alloc = malloc;
    allocator.Free = free;

    gpr_parameters params;
    gpr_parameters_set_defaults(&params);

    gpr_buffer input = {NULL, 0};
    gpr_buffer output = {NULL, 0};
    int rc = -2;

    if (read_from_file(&input, in_path, malloc, free) == 0) {
        // parses EXIF/tuning metadata needed for the DNG write; proceed
        // even when it fails so permissive files still convert.
        gpr_parse_metadata(&allocator, &input, &params);
        if (gpr_convert_gpr_to_dng(&allocator, &params, &input, &output) &&
            output.buffer && output.size > 0 &&
            write_to_file(&output, out_path) == 0) {
            rc = 0;
        }
    }

    if (output.buffer)
        free(output.buffer);
    if (input.buffer)
        free(input.buffer);
    gpr_parameters_destroy(&params, free);
    return rc;
}
