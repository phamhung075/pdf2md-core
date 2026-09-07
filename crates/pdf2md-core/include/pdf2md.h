/*
 * Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
 * SPDX-License-Identifier: BSL-1.1
 * Licensed under the Business Source License 1.1 (BSL-1.1).
 */

#ifndef PDF2MD_H
#define PDF2MD_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Converts raw PDF bytes to Markdown.
 *
 * Returns a heap-allocated JSON C string (caller frees with pdf2md_free_string):
 *   { "ok": true,  "markdown": "...", "pages": N, "words": N, "tables": N, "duration_us": N }
 *   { "ok": false, "error": "..." }
 */
char *pdf2md_convert(const uint8_t *pdf_bytes, size_t pdf_len);

/* Same as pdf2md_convert, but with explicit vector figure detection flag. */
char *pdf2md_convert_ex(const uint8_t *pdf_bytes, size_t pdf_len, int detect_vectors);

/* Returns 1 when the PDF has a digital text layer, 0 otherwise. */
int pdf2md_is_digital(const uint8_t *pdf_bytes, size_t pdf_len);

/* Frees a string returned by pdf2md_convert / pdf2md_version. */
void pdf2md_free_string(char *ptr);

/* Returns the engine version as a heap-allocated C string (caller frees it). */
char *pdf2md_version(void);

#ifdef __cplusplus
}
#endif

#endif /* PDF2MD_H */
