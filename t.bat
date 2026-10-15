@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
cd /d E:\dev\safelight\core
cl -nologo -MD -Z7 -I../third_party/gpr/gpr_sdk/public -I../third_party/gpr/common/public -I../third_party/gpr/common/private -I../third_party/gpr/vc5_common -I../third_party/gpr/vc5_decoder -I../third_party/gpr/dng_sdk -I../third_party/gpr/md5_lib -I../third_party/gpr/expat_lib -I../third_party/gpr/xmp_core/public/include -Inative -W0 /FI E:\dev\safelight\core\native\msvc_compat.h -DGPR_READING=1 -DGPR_WRITING=0 -DGPR_JPEG_AVAILABLE=0 -DGPR_TIMING=0 -DXML_STATIC=1 -DqWinOS=1 -c %1 -FoE:\tmp\out.o
