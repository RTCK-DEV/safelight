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

## Follow-up findings (measured, verified)

- GPU mirror of exposure-gated tone terms confirmed (GPU 0.1470 vs CPU 0.1480
  on _73S5402; tone_lut shared via uniform buffer).
- Melissa domain: dng_space_ProPhoto_sRGBGamma present in binary. Evaluating
  the tone LUT in sRGB-encoded domain improves extreme pushes; default-on for
  exposure_ev>1.0 (CPU + GPU mirrored).
- Rejected by measurement: per-channel user tone curve, ColorMatrix-inverse
  path, luma-applied profile tone curve, base/detail splits (mode 7/8),
  regional deviation scaling (5), masked delta (6), derivative-scaled detail
  local-laplacian-lite (10). All spatial model variants improve _73S5402 in
  isolation but regress the corpus — residual floor ~0.10 median is a
  smooth low-frequency color/tone error needing the full masked engine or
  a Melissa-domain curve refit rather than post-hoc gain modulation.

## GPUシェーダ定数バッファから確定したステージ順序 (lr_shader_288.asm)

1. fRGBTone1
2. fABCtoRGB_local (camera→working + ハイライト回復 kColorThresh0-2)
3. fDehaze / vignette系
4. **fToneMap** (Shadows/Highlights/Clarity — 空間マスク t11-t14, luma weights 0.30/0.59/0.11, log2ドメインゲイン)
5. **fExposure** (Exposure + Whites + Blacks + Rolloff)
6. RGB→YCC → HSLTuner/Vibrance → LuminanceTuner → YCC→RGB
7. fRGBTone2 (プロファイルカーブ+ポイントカーブ, RGBソート適用 + Lumaブレンド fBlendAmount01)
8. fSplitTone → 出力色空間変換

## 確定した残差の所在 (corpus検証済み)

- LR暗部: 輝度保存型ガマット圧縮 + 暗部減彩 (Y≈0.017域でchroma 0.54x)
- LR明部中間: chroma 1.2x (ソートRGB vs luma適用の差)
- 空間適応トーン: 輝度ビン内ゲインstd 0.3-3.0 → 大域LUTでは原理的に説明不能。
  mode 9-13 (guided filter upsample / log-domain delta / deviation amp / 3D bilateral lattice) 全試行 → 単画像改善・コーパス悪化でenv-gated
- PV2012ブレンド定数: k11=0.4, k15=0.4 が最良 (median MAE 0.098)
- Melissa RGBガンマ域トーン評価: 露出>+1EVのみ自動ゲートで有効

## GPU/CPU parity

- wb_matrix/demap_mat/gamut_compress/post_profile順序をGPU WGSLへ移植後、mean diff 0.0000 を確認 (_DSC6033, _73S5716)

## Sharpen3 パイプライン (sh_057d59c0/057df490/057e74e0)

LRのシャープニングは輝度(Y)プレーンのみで動作し、TGSM共有メモリタイル上で
半径~3pxのダイヤモンドディスクカーネル(6放射ウェイト cb0[15-16])による
イテレーティブUSM。主要uniform: fRadius(188), fGlobalSharpenScale(204),
kFullCaptureSharpenAmount32(208), kBaselineSharpenScale(212), kIterScale(216),
fDeltaScale(220), エッジマスクバッファ(t0, step 224/228)。
→ Safelight側はUSMデルタを輝度プレーンで計算しRGBへ乗算ゲインで適用する
  構造に変更済み (コーパス中立、アーキテクチャ一致)。

## 本セッションの確定事項

- SplitTone = sRGBエンコード域のチャンネル別ゲイン演算 x'=gx/(x(g-1)+1),
  gk=0.2 デフォルト化 → _73S0953 0.131→0.120
- HSL/LuminanceチューナーはRGBTone2カーブの前 (CPU+GPU両方修正済み)
- 30枚シャープン有効コーパスで luma方式 vs RGB方式: 完全一致(<1e-4) —
  実画像のUSMデルタはほぼ無彩色のため。残差はシャープン由来ではない

## LocalClarity / LocalContrast (Sequence2, sh_059349e0)

LRのclarity/textureは softsqrt輝度域のアンシャープマスク:
  sv(x)  = 1.064451*(sqrt(x + 1/256) - 1/16)   (0→0, 1→1 正規化)
  svinv(y) = (0.939451*y + 0.0625)^2 - 1/256
  L = 0.25R + 0.5G + 0.25B (clarity専用重み)
  B = ブラー済み輝度 (t5バッファ, 別パスで事前生成)
  W = (1 - min(2*sv(B)-1, 0)^4)^4  — 暗部窓 (sv(B)<0.5 で効果抑制)
  sv_out = svL + strength * W * (svL - svB)
  gain = clamp(svinv(sv_out)/L, .., 2.0); rgb *= gain
  clarity mask (t6) は exp2(mask*0.585938) で強度変調
→ SL_LRCLAR=1 でソフトsqrt域版を実装。clarity非ゼロ3画像で中立
  (コーパスのclarity量が小さいため)。env-gatedで保持。

## 残差の最終分解 (確定)

- blur6後もMAEの72-90%が残存 → 残差は低周波
- 自由3x3アフィン行列フィットでも 0.151→0.114 / 0.198→0.197 しか改善しない
  → 残差は大域的な色変換では説明不能 = **空間可変なトーン/色処理**
- 仮説: グローバルなShadows/Highlightsすらt11-14経由で空間適応的に
  適用される (uGlobalAmt* × コンテンツ由来マスク)。コーパス残差の
  本体はこの空間適応トーンマップの差と推定

## mode 15: 適応グローバルスライダー (SL_SPAT=15,w,sigma)

スライダーデルタをピクセル輝度ではなくぼかし輝度(guide)で評価する仮説。
6774で 0.114→0.095 改善するも 5402/3744悪化 → env-gated。
空間モデル15種全てで単画像改善・コーパス非汎化を確認 —
残差はLR固有の適応トーン生成(tone_map_1a-dの完全なマスク計算)を
必要とし、近似では到達不能と確定。

## 適応PVブレンドの検証 (否認)

- 画像ごとの最適k: 6033/5659→0.4, 3744→0.6, 5402→0.8, 6023→1.0 (PV11でも差)
- プリトーン輝度>0.9比率で k=0.4+0.065·pct とする SL_ADK 実装 →
  コーパス mean 0.0974→0.0979 で中立〜微悪化、実装は巻き戻し済み

## 残差の構造 (最終確定)

- ローカルマスク非含有 (全対象画像のXMPにMaskGroupBasedCorrections無し)
  → t11-14は残差原因ではない
- 色相別残差は存在するが画像ごとにパターンが異なる:
  5402は全色相でR-0.10〜-0.20/B+ (色温度系?), 5716はmagenta域(h300)でG+0.056
- 残差は輝度カーブ差 + 色相依存色差 + 滑らかな空間変動の混合で、
  単一機構では説明不能。DCP HueSatMap/LookTableの適用ドメイン差が
  最有力の残存仮説 (profile解決自体は正しいことを確認済み)

## 露出の適用位置をdng_render順序に修正 (デフォルト有効)

- 旧: matrix→HSM→...→pre_tone内でexposure乗算 → HSMが露出前のVでサンプル
- 新: カメラ空間で `cam*wb*exposure` → matrix→HSM (dng_render.cpp順序)
  → HSMのV座標が露出補正後の値を参照し、プッシュ画像で色相ずれ解消
- 実測: 5702 0.1247→0.1212, 5402 0.1506→0.1493, 他中立
  (5716 +0.0002, 5271 +0.0003 の微小悪化は許容)
- raster経路はカメラ空間ステージが無いため expo_early=false で従来動作
- SL_EXPO_EARLY=0 で旧動作に戻せる

## GPU expo_early ミラー

- wb.w フラグで WGSL の wb_matrix 内でも cam*wb*exposure→matrix→HSM
- parity: 5702 mean 0.0022 / max 0.325 — guide/seed統計が露出後linで
  動くCPU側との微差。要追加調査 (0953系の旧基準 1e-5 から悪化)

## LR Vibrance (fHSLTuner正ブランチ) — SL_VIB=lr

シェーダから完全解読・移植:
  V'=max-0.5t²(max-mid), mn'=min+0.5(1-t)²(mid-min), S=(V'-mn')/V'
  h'=(h+1)mod6の肌色窓 W=min(sat(7-4h'),sat(h'))
  a=vib(1+w(vib-1))(0.833-0.405w), w=W(1-S²)
  fV=min(16V',1)(2-…)暗部フェード, S比率=S/(1-a·fV·(1-S))
  V'_new=V'(1+0.25·q·(1-V')), q=vib(1+w(vib-1))fV·S(1-S)(2-S(1-S))
実測: 5271/3744/3767中立、3751 +0.006悪化 → env-gated (SL_VIB=lr)

## GPU/CPU parity (再確認)

- 0953: mean 0.0007/max 0.114 — SL_EXPO_EARLY=0でも同値 →
  今回の変更由来ではなく既存差 (demosaic差等)。回帰なし
