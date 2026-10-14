# Lightroom Classic tone engine — recovered pipeline (CameraRaw.dll 15.6, 162MB)

## Adaptive tone stages (symbol names mined from binary)

Base tone map (image-adaptive, computed at LOW res, edge-aware upsampled):
- AgCRContext:buildToneMapMask, DownsampleToneMapMask, UpsampleToneMapMask, ToneMapMaskBuf
- cr_stage_tone_map_1a/1b/1c/1d, cr_stage_upsample_tone_map_1/2
- cr_stage_bilateral_downsample_3D, cr_stage_bilateral_upsample, cr_stage_bilateral_blur
- cr_base_tone_map_data / cr_tone_map_warped|unwarped_cache_request
- base_tone_map_cache_strength_%u  (mask cached per strength value)
- cr_find_lookahead_region_op

Slider-specific spatial machinery:
- cr_stage_fill_light_32/{mask,local,fill}  + cr_fill_light_mask_{warped,unwarped}_cache
- cr_stage_highlight_guide + cr_stage_highlight_guide_gradient_mask + _highlight_mask
- cr_stage_local_whites_blacks, cr_stage_local_contrast, cr_stage_LocalContrastY
- cr_stage_localized_detail, cr_stage_LocalColorToning, cr_stage_LocalHue

Dehaze (dark-channel prior confirmed):
- DehazeMin1Pass1PlanePlusDarkChannelPreprocess, DehazeMinFilter...AcrossPlusDarkChannel
- DehazeEstimateAirlight1/2/3, DehazeTrMDarkChannel, DehazeTrMEmitIndexedRGBL
- BuildDehazeSource -- "PV4 and earlier" vs "PV5 and later" variants

Laplacian pyramid:
- cr_laplacian_pyramid, cr_gaussian_pyramid, cr_gpu_pyramid, cr_stage_pyramid
- cr_stage_laplacian_render_incremental ("reduce"/"expand"/"gaussian" buffers)

Tone curve objects:
- cr_concat_parametric_curves, cr_curve_parametric, cr_curve_contrast
- cr_sdr_linear_tone_curve, cr_hdr_tone_curve_accelerator, cr_tone_stage_curve2
- ExtendedToneCurvePV2012{,Red,Green,Blue}, ToneCurvePV2012*, ToneCurveLabelPV2012
- ParametricMidtoneSplit, RSPish_MidtoneSplit, RSPish_HighlightRecovery
- "Exposure Rolloff Extra Stops: %.8lf", "Rolloff Extra Slope(Sqr)", "LShoulder"/"RShoulder"
- SimpleToneMap / UniformsSimpleToneMap (GPU), UniformsToneMap

Auto SDR (HDR tone mapping): "m09_local_contrast_brightness_floor", toneMapNeed scoring.

## Architecture conclusion

LR's Shadows/Highlights/Whites/Blacks are NOT a global LUT: a base tone-map
mask is computed on a downsampled image (bilateral 3D downsample), slider
deltas are evaluated per-region, then the mask is edge-aware upsampled
(bilateral/guided-filter, cr_stage_guided_filter_{affine,ycc,rgb}) and applied.
This is the fast-local-laplacian family: coarse adaptive tone + joint
edge-preserving upsample. Fill light and highlight recovery have their own
mask/guide caches warped by geometry (crop/upright).
