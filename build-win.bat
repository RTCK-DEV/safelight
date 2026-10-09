@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
set LIBRAW_SRC=E:\dev\libraw-src\LibRaw-0.22.2
cargo %*
