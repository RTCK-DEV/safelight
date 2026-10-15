/* Neutralize GNU-only attribute syntax when compiling vendored C/C++
 * sources with MSVC. Force-included via /FI in build.rs on Windows. */
#pragma once
#ifdef _MSC_VER
#ifndef __attribute
#define __attribute(x)
#endif
#ifndef __attribute__
#define __attribute__(x)
#endif
#endif
