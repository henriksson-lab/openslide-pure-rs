#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <limits.h>
#include <jpeglib.h>
#include <setjmp.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

struct osr_jpeg_error {
    struct jpeg_error_mgr pub;
    jmp_buf setjmp_buffer;
    char message[JMSG_LENGTH_MAX];
};

static void osr_jpeg_error_exit(j_common_ptr cinfo) {
    struct osr_jpeg_error *err = (struct osr_jpeg_error *)cinfo->err;
    (*cinfo->err->format_message)(cinfo, err->message);
    longjmp(err->setjmp_buffer, 1);
}

static void osr_set_error(char *err, size_t err_len, const char *message) {
    if (err == NULL || err_len == 0) {
        return;
    }
    snprintf(err, err_len, "%s", message);
}

static long osr_floor_to_long(double value) {
    long truncated = (long)value;
    return value < (double)truncated ? truncated - 1 : truncated;
}

static int osr_jpeg_scale_denom(double sample_step) {
    const double eps = 1e-9;
    if (sample_step > 2.0 - eps && sample_step < 2.0 + eps) {
        return 2;
    }
    if (sample_step > 4.0 - eps && sample_step < 4.0 + eps) {
        return 4;
    }
    if (sample_step > 8.0 - eps && sample_step < 8.0 + eps) {
        return 8;
    }
    return 0;
}

int osr_jpeg_memory_range_rgb(unsigned char *buffer,
                              size_t total_len,
                              unsigned long long sof_offset,
                              unsigned int tile_w,
                              unsigned int tile_h,
                              unsigned int scale_denom,
                              unsigned int expected_w,
                              unsigned int expected_h,
                              unsigned char *out,
                              char *err,
                              size_t err_len);

static JDIMENSION osr_jpeg_div_round_up(unsigned long value, unsigned long divisor) {
    return (JDIMENSION)((value + divisor - 1) / divisor);
}

int osr_jpeg_lossless_crop(const unsigned char *data,
                           size_t len,
                           unsigned int x,
                           unsigned int y,
                           unsigned int w,
                           unsigned int h,
                           unsigned char **out,
                           unsigned long *out_len,
                           char *err,
                           size_t err_len) {
    struct jpeg_decompress_struct srcinfo;
    struct jpeg_compress_struct dstinfo;
    struct osr_jpeg_error srcerr;
    struct osr_jpeg_error dsterr;
    jvirt_barray_ptr *src_coef_arrays = NULL;
    jvirt_barray_ptr *dst_coef_arrays = NULL;
    unsigned char *mem = NULL;
    unsigned long mem_len = 0;
    int result = 0;
    int dst_created = 0;

    if (data == NULL || out == NULL || out_len == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG lossless crop argument");
        return 0;
    }
    *out = NULL;
    *out_len = 0;
    if (w == 0 || h == 0) {
        osr_set_error(err, err_len, "JPEG lossless crop rectangle is empty");
        return 0;
    }

    memset(&srcinfo, 0, sizeof(srcinfo));
    memset(&dstinfo, 0, sizeof(dstinfo));
    srcinfo.err = jpeg_std_error(&srcerr.pub);
    srcerr.pub.error_exit = osr_jpeg_error_exit;
    dstinfo.err = jpeg_std_error(&dsterr.pub);
    dsterr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(srcerr.setjmp_buffer)) {
        osr_set_error(err, err_len, srcerr.message);
        goto finish;
    }
    if (setjmp(dsterr.setjmp_buffer)) {
        osr_set_error(err, err_len, dsterr.message);
        goto finish;
    }

    jpeg_create_decompress(&srcinfo);
    jpeg_mem_src(&srcinfo, data, (unsigned long)len);
    jpeg_read_header(&srcinfo, TRUE);
    src_coef_arrays = jpeg_read_coefficients(&srcinfo);

    if ((unsigned long)x + (unsigned long)w > srcinfo.image_width ||
        (unsigned long)y + (unsigned long)h > srcinfo.image_height) {
        osr_set_error(err, err_len, "JPEG lossless crop rectangle is outside image bounds");
        goto finish;
    }
    if (srcinfo.max_h_samp_factor <= 0 || srcinfo.max_v_samp_factor <= 0) {
        osr_set_error(err, err_len, "JPEG lossless crop has invalid sampling factors");
        goto finish;
    }
    unsigned int mcu_w = (unsigned int)srcinfo.max_h_samp_factor * DCTSIZE;
    unsigned int mcu_h = (unsigned int)srcinfo.max_v_samp_factor * DCTSIZE;
    if (x % mcu_w != 0 || y % mcu_h != 0) {
        osr_set_error(err, err_len, "JPEG lossless crop origin is not MCU-aligned");
        goto finish;
    }
    if (((unsigned long)x + (unsigned long)w < srcinfo.image_width && w % mcu_w != 0) ||
        ((unsigned long)y + (unsigned long)h < srcinfo.image_height && h % mcu_h != 0)) {
        osr_set_error(err, err_len, "JPEG lossless crop size is not MCU-compatible");
        goto finish;
    }

    jpeg_create_compress(&dstinfo);
    dst_created = 1;
    jpeg_mem_dest(&dstinfo, &mem, &mem_len);
    jpeg_copy_critical_parameters(&srcinfo, &dstinfo);
    dstinfo.image_width = w;
    dstinfo.image_height = h;
    dstinfo.jpeg_width = w;
    dstinfo.jpeg_height = h;

    dst_coef_arrays = (jvirt_barray_ptr *)(*dstinfo.mem->alloc_small)(
        (j_common_ptr)&dstinfo,
        JPOOL_IMAGE,
        srcinfo.num_components * sizeof(jvirt_barray_ptr));
    for (int ci = 0; ci < srcinfo.num_components; ci++) {
        jpeg_component_info *srccompptr = srcinfo.comp_info + ci;
        jpeg_component_info *dstcompptr = dstinfo.comp_info + ci;
        JDIMENSION dst_width_blocks = osr_jpeg_div_round_up(
            (unsigned long)dstinfo.jpeg_width * (unsigned long)dstcompptr->h_samp_factor,
            (unsigned long)srcinfo.max_h_samp_factor * DCTSIZE);
        JDIMENSION dst_height_blocks = osr_jpeg_div_round_up(
            (unsigned long)dstinfo.jpeg_height * (unsigned long)dstcompptr->v_samp_factor,
            (unsigned long)srcinfo.max_v_samp_factor * DCTSIZE);
        dstcompptr->width_in_blocks = dst_width_blocks;
        dstcompptr->height_in_blocks = dst_height_blocks;
        dst_coef_arrays[ci] = (*dstinfo.mem->request_virt_barray)(
            (j_common_ptr)&dstinfo,
            JPOOL_IMAGE,
            TRUE,
            dst_width_blocks,
            dst_height_blocks,
            (JDIMENSION)dstcompptr->v_samp_factor);
        (void)srccompptr;
    }

    jpeg_write_coefficients(&dstinfo, dst_coef_arrays);
    for (int ci = 0; ci < srcinfo.num_components; ci++) {
        jpeg_component_info *srccompptr = srcinfo.comp_info + ci;
        jpeg_component_info *dstcompptr = dstinfo.comp_info + ci;
        JDIMENSION src_col = (JDIMENSION)((unsigned long)x / mcu_w *
                                         (unsigned long)srccompptr->h_samp_factor);
        JDIMENSION src_row = (JDIMENSION)((unsigned long)y / mcu_h *
                                         (unsigned long)srccompptr->v_samp_factor);
        for (JDIMENSION row = 0; row < dstcompptr->height_in_blocks; row++) {
            JBLOCKARRAY src_buffer = (*srcinfo.mem->access_virt_barray)(
                (j_common_ptr)&srcinfo,
                src_coef_arrays[ci],
                src_row + row,
                1,
                FALSE);
            JBLOCKARRAY dst_buffer = (*dstinfo.mem->access_virt_barray)(
                (j_common_ptr)&dstinfo,
                dst_coef_arrays[ci],
                row,
                1,
                TRUE);
            memcpy(dst_buffer[0],
                   src_buffer[0] + src_col,
                   (size_t)dstcompptr->width_in_blocks * sizeof(JBLOCK));
        }
    }

    jpeg_finish_compress(&dstinfo);
    jpeg_finish_decompress(&srcinfo);
    *out = mem;
    *out_len = mem_len;
    mem = NULL;
    result = 1;

finish:
    if (dst_created) {
        jpeg_destroy_compress(&dstinfo);
    }
    jpeg_destroy_decompress(&srcinfo);
    if (mem != NULL) {
        free(mem);
    }
    return result;
}

void osr_jpeg_free_buffer(unsigned char *buffer) {
    free(buffer);
}

int osr_jpeg_encode_rgb_for_test(const unsigned char *rgb,
                                 unsigned int width,
                                 unsigned int height,
                                 unsigned int quality,
                                 unsigned char **out,
                                 unsigned long *out_len,
                                 char *err,
                                 size_t err_len) {
    struct jpeg_compress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPROW row_pointer[1];
    unsigned char *mem = NULL;
    unsigned long mem_len = 0;
    int result = 0;

    if (rgb == NULL || out == NULL || out_len == NULL || width == 0 || height == 0) {
        osr_set_error(err, err_len, "invalid null JPEG encode argument");
        return 0;
    }
    *out = NULL;
    *out_len = 0;

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_compress(&cinfo);
        if (mem != NULL) {
            free(mem);
        }
        return 0;
    }

    jpeg_create_compress(&cinfo);
    jpeg_mem_dest(&cinfo, &mem, &mem_len);
    cinfo.image_width = width;
    cinfo.image_height = height;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_RGB;
    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, quality > 100 ? 100 : (int)quality, TRUE);
    jpeg_start_compress(&cinfo, TRUE);
    while (cinfo.next_scanline < cinfo.image_height) {
        row_pointer[0] = (JSAMPROW)&rgb[(size_t)cinfo.next_scanline * width * 3];
        jpeg_write_scanlines(&cinfo, row_pointer, 1);
    }
    jpeg_finish_compress(&cinfo);
    jpeg_destroy_compress(&cinfo);
    *out = mem;
    *out_len = mem_len;
    result = 1;
    return result;
}

int osr_jpeg_crop_channel(const unsigned char *data,
                          size_t len,
                          unsigned int channel,
                          unsigned int x,
                          unsigned int y,
                          unsigned int w,
                          unsigned int h,
                          unsigned char *out,
                          char *err,
                          size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG crop argument");
        return 0;
    }
    if (channel > 2) {
        osr_set_error(err, err_len, "invalid JPEG crop channel");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height) {
        osr_set_error(err, err_len, "JPEG crop rectangle is outside image bounds");
        goto finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG crop scanline read failed");
            goto finish;
        }
        unsigned char *src = rows[0] + left * cinfo.output_components + channel;
        unsigned char *dst = out + (size_t)row * w;
        for (unsigned int col = 0; col < w; col++) {
            dst[col] = src[(size_t)col * cinfo.output_components];
        }
    }

    result = 1;

finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_decode_rgb(const unsigned char *data,
                        size_t len,
                        unsigned int expected_w,
                        unsigned int expected_h,
                        unsigned char *out,
                        char *err,
                        size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG RGB decode argument");
        return 0;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if (cinfo.output_width != expected_w || cinfo.output_height != expected_h ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "JPEG RGB dimensions/components did not match expected output");
        goto rgb_finish;
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    while (cinfo.output_scanline < cinfo.output_height) {
        JDIMENSION row = cinfo.output_scanline;
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG RGB scanline read failed");
            goto rgb_finish;
        }
        memcpy(out + (size_t)row * expected_w * 3,
               rows[0],
               (size_t)expected_w * 3);
    }

    result = 1;

rgb_finish:
    if (result) {
        jpeg_finish_decompress(&cinfo);
    } else {
        jpeg_abort_decompress(&cinfo);
    }
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_dimensions(const unsigned char *data,
                        size_t len,
                        unsigned int *width,
                        unsigned int *height,
                        char *err,
                        size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    int result = 0;

    if (data == NULL || width == NULL || height == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG dimensions argument");
        return 0;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    if (jpeg_read_header(&cinfo, TRUE) != JPEG_HEADER_OK) {
        osr_set_error(err, err_len, "Couldn't read JPEG header");
        goto dimensions_finish;
    }
    jpeg_calc_output_dimensions(&cinfo);
    *width = cinfo.output_width;
    *height = cinfo.output_height;
    result = 1;

dimensions_finish:
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_decode_tiff_ycbcr_rgb(const unsigned char *data,
                                   size_t len,
                                   unsigned int expected_w,
                                   unsigned int expected_h,
                                   unsigned char *out,
                                   char *err,
                                   size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null TIFF YCbCr JPEG decode argument");
        return 0;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.jpeg_color_space = JCS_YCbCr;
    cinfo.out_color_space = JCS_EXT_BGRA;
    jpeg_start_decompress(&cinfo);

    if (cinfo.output_width != expected_w || cinfo.output_height != expected_h ||
        cinfo.output_components != 4) {
        osr_set_error(err, err_len, "TIFF YCbCr JPEG dimensions/components did not match expected output");
        goto tiff_ycbcr_finish;
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    while (cinfo.output_scanline < cinfo.output_height) {
        JDIMENSION row = cinfo.output_scanline;
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "TIFF YCbCr JPEG scanline read failed");
            goto tiff_ycbcr_finish;
        }
        unsigned char *dst = out + (size_t)row * expected_w * 3;
        for (unsigned int col = 0; col < expected_w; col++) {
            unsigned char *src = rows[0] + (size_t)col * 4;
            dst[(size_t)col * 3 + 0] = src[2];
            dst[(size_t)col * 3 + 1] = src[1];
            dst[(size_t)col * 3 + 2] = src[0];
        }
    }

    result = 1;

tiff_ycbcr_finish:
    if (result) {
        jpeg_finish_decompress(&cinfo);
    } else {
        jpeg_abort_decompress(&cinfo);
    }
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_file_range_rgb(const char *path,
                            unsigned long long header_start,
                            unsigned long long sof_position,
                            unsigned long long header_stop,
                            unsigned long long data_start,
                            unsigned long long data_stop,
                            unsigned int tile_w,
                            unsigned int tile_h,
                            unsigned int scale_denom,
                            unsigned int expected_w,
                            unsigned int expected_h,
                            unsigned char *out,
                            char *err,
                            size_t err_len) {
    FILE *file = NULL;
    unsigned char *buffer = NULL;
    int result = 0;

    if (path == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG range argument");
        return 0;
    }
    if (tile_w == 0 || tile_h == 0 || expected_w == 0 || expected_h == 0) {
        return 1;
    }
    if (header_start > header_stop || header_stop > data_start || data_start > data_stop) {
        osr_set_error(err, err_len, "invalid JPEG range offsets");
        return 0;
    }
    if (header_start > (unsigned long long)LONG_MAX ||
        sof_position > (unsigned long long)LONG_MAX ||
        header_stop > (unsigned long long)LONG_MAX ||
        data_start > (unsigned long long)LONG_MAX ||
        data_stop > (unsigned long long)LONG_MAX) {
        osr_set_error(err, err_len, "JPEG range offset is too large");
        return 0;
    }

    unsigned long long header_len_u64 = header_stop - header_start;
    unsigned long long data_len_u64 = data_stop - data_start;
    unsigned long long total_len_u64 = header_len_u64 + data_len_u64;
    if (header_len_u64 > (unsigned long long)SIZE_MAX ||
        data_len_u64 > (unsigned long long)SIZE_MAX ||
        total_len_u64 > (unsigned long long)SIZE_MAX ||
        total_len_u64 < 2) {
        osr_set_error(err, err_len, "JPEG range is too large");
        return 0;
    }

    size_t header_len = (size_t)header_len_u64;
    size_t data_len = (size_t)data_len_u64;
    size_t total_len = (size_t)total_len_u64;

    file = fopen(path, "rb");
    if (file == NULL) {
        osr_set_error(err, err_len, "failed to open JPEG file");
        return 0;
    }

    buffer = (unsigned char *)malloc(total_len);
    if (buffer == NULL) {
        osr_set_error(err, err_len, "failed to allocate JPEG range buffer");
        fclose(file);
        return 0;
    }

    if (fseek(file, (long)header_start, SEEK_SET) != 0 ||
        fread(buffer, 1, header_len, file) != header_len ||
        fseek(file, (long)data_start, SEEK_SET) != 0 ||
        fread(buffer + header_len, 1, data_len, file) != data_len) {
        osr_set_error(err, err_len, "failed to read JPEG range");
        goto range_finish;
    }

    result = osr_jpeg_memory_range_rgb(buffer,
                                       total_len,
                                       sof_position - header_start,
                                       tile_w,
                                       tile_h,
                                       scale_denom,
                                       expected_w,
                                       expected_h,
                                       out,
                                       err,
                                       err_len);

range_finish:
    free(buffer);
    fclose(file);
    return result;
}

int osr_jpeg_memory_range_rgb(unsigned char *buffer,
                              size_t total_len,
                              unsigned long long sof_offset,
                              unsigned int tile_w,
                              unsigned int tile_h,
                              unsigned int scale_denom,
                              unsigned int expected_w,
                              unsigned int expected_h,
                              unsigned char *out,
                              char *err,
                              size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (buffer == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG memory range argument");
        return 0;
    }
    if (tile_w == 0 || tile_h == 0 || expected_w == 0 || expected_h == 0) {
        return 1;
    }
    if (total_len < 2) {
        osr_set_error(err, err_len, "JPEG memory range is too small");
        return 0;
    }
    if (buffer[total_len - 2] != 0xff) {
        osr_set_error(err, err_len, "JPEG range does not end at a marker");
        return 0;
    }
    buffer[total_len - 1] = JPEG_EOI;

    unsigned long long size_offset_u64 = sof_offset + 5;
    if (size_offset_u64 + 4 > (unsigned long long)total_len) {
        osr_set_error(err, err_len, "JPEG SOF is outside header range");
        return 0;
    }
    size_t size_offset = (size_t)size_offset_u64;
    buffer[size_offset + 0] = (unsigned char)((tile_h >> 8) & 0xff);
    buffer[size_offset + 1] = (unsigned char)(tile_h & 0xff);
    buffer[size_offset + 2] = (unsigned char)((tile_w >> 8) & 0xff);
    buffer[size_offset + 3] = (unsigned char)(tile_w & 0xff);

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, buffer, (unsigned long)total_len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    if (scale_denom > 1) {
        cinfo.scale_num = 1;
        cinfo.scale_denom = scale_denom;
    }
    cinfo.image_width = tile_w;
    cinfo.image_height = tile_h;
    jpeg_start_decompress(&cinfo);

    if (cinfo.output_width != expected_w ||
        cinfo.output_height != expected_h ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "JPEG range output dimensions/components mismatch");
        goto range_finish;
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    for (unsigned int row = 0; row < expected_h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG range scanline read failed");
            goto range_finish;
        }
        memcpy(out + (size_t)row * expected_w * 3,
               rows[0],
               (size_t)expected_w * 3);
    }

    result = 1;

range_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_crop_rgb(const unsigned char *data,
                      size_t len,
                      unsigned int x,
                      unsigned int y,
                      unsigned int w,
                      unsigned int h,
                      unsigned char *out,
                      char *err,
                      size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG RGB crop argument");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "JPEG RGB crop rectangle/components are invalid");
        goto rgb_crop_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto rgb_crop_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto rgb_crop_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG RGB crop scanline read failed");
            goto rgb_crop_finish;
        }
        memcpy(out + (size_t)row * w * 3,
               rows[0] + (size_t)left * cinfo.output_components,
               (size_t)w * 3);
    }

    result = 1;

rgb_crop_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_crop_bgra_rgb(const unsigned char *data,
                           size_t len,
                           unsigned int x,
                           unsigned int y,
                           unsigned int w,
                           unsigned int h,
                           int jpeg_color_space,
                           unsigned char *out,
                           char *err,
                           size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG BGRA crop argument");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    if (jpeg_color_space == 1) {
        cinfo.jpeg_color_space = JCS_RGB;
    } else if (jpeg_color_space == 2) {
        cinfo.jpeg_color_space = JCS_YCbCr;
    }
    cinfo.out_color_space = JCS_EXT_BGRA;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height ||
        cinfo.output_components != 4) {
        osr_set_error(err, err_len, "JPEG BGRA crop rectangle/components are invalid");
        goto bgra_crop_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto bgra_crop_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto bgra_crop_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG BGRA crop scanline read failed");
            goto bgra_crop_finish;
        }
        unsigned char *src = rows[0] + (size_t)left * cinfo.output_components;
        unsigned char *dst = out + (size_t)row * w * 3;
        for (unsigned int col = 0; col < w; col++) {
            dst[(size_t)col * 3 + 0] = src[(size_t)col * 4 + 2];
            dst[(size_t)col * 3 + 1] = src[(size_t)col * 4 + 1];
            dst[(size_t)col * 3 + 2] = src[(size_t)col * 4 + 0];
        }
    }

    result = 1;

bgra_crop_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_tiff_bgra_crop_rgb(const unsigned char *data,
                                size_t len,
                                const unsigned char *tables,
                                size_t tables_len,
                                unsigned int x,
                                unsigned int y,
                                unsigned int w,
                                unsigned int h,
                                int jpeg_color_space,
                                unsigned char *out,
                                char *err,
                                size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    int result = 0;

    if (data == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null TIFF JPEG RGB crop argument");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    if (tables != NULL && tables_len > 0) {
        jpeg_mem_src(&cinfo, tables, (unsigned long)tables_len);
        if (jpeg_read_header(&cinfo, FALSE) != JPEG_HEADER_TABLES_ONLY) {
            osr_set_error(err, err_len, "failed to load TIFF JPEG tables");
            goto tiff_bgra_crop_finish;
        }
    }
    jpeg_mem_src(&cinfo, data, (unsigned long)len);
    jpeg_read_header(&cinfo, TRUE);
    if (jpeg_color_space == 1) {
        cinfo.jpeg_color_space = JCS_RGB;
    } else if (jpeg_color_space == 2) {
        cinfo.jpeg_color_space = JCS_YCbCr;
    }
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "TIFF JPEG RGB crop rectangle/components are invalid");
        goto tiff_bgra_crop_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid TIFF JPEG crop rectangle");
        goto tiff_bgra_crop_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "TIFF JPEG vertical crop skip failed");
            goto tiff_bgra_crop_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "TIFF JPEG BGRA crop scanline read failed");
            goto tiff_bgra_crop_finish;
        }
        unsigned char *src = rows[0] + (size_t)left * cinfo.output_components;
        unsigned char *dst = out + (size_t)row * w * 3;
        memcpy(dst, src, (size_t)w * 3);
    }

    result = 1;

tiff_bgra_crop_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return result;
}

int osr_jpeg_file_crop_channel(const char *path,
                               unsigned long long offset,
                               unsigned int channel,
                               unsigned int x,
                               unsigned int y,
                               unsigned int w,
                               unsigned int h,
                               unsigned char *out,
                               char *err,
                               size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    FILE *file = NULL;
    int result = 0;

    if (path == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG file crop argument");
        return 0;
    }
    if (channel > 2) {
        osr_set_error(err, err_len, "invalid JPEG crop channel");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        if (file != NULL) {
            fclose(file);
        }
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    file = fopen(path, "rb");
    if (file == NULL) {
        osr_set_error(err, err_len, "failed to open JPEG file");
        return 0;
    }
    if (offset > (unsigned long long)LONG_MAX || fseek(file, (long)offset, SEEK_SET) != 0) {
        osr_set_error(err, err_len, "failed to seek JPEG file");
        fclose(file);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_stdio_src(&cinfo, file);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height) {
        osr_set_error(err, err_len, "JPEG crop rectangle is outside image bounds");
        goto file_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto file_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto file_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG crop scanline read failed");
            goto file_finish;
        }
        unsigned char *src = rows[0] + left * cinfo.output_components + channel;
        unsigned char *dst = out + (size_t)row * w;
        for (unsigned int col = 0; col < w; col++) {
            dst[col] = src[(size_t)col * cinfo.output_components];
        }
    }

    result = 1;

file_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    fclose(file);
    return result;
}

int osr_jpeg_file_crop_rgb(const char *path,
                           unsigned long long offset,
                           unsigned int x,
                           unsigned int y,
                           unsigned int w,
                           unsigned int h,
                           unsigned char *out,
                           char *err,
                           size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    FILE *file = NULL;
    int result = 0;

    if (path == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG RGB crop argument");
        return 0;
    }
    if (w == 0 || h == 0) {
        return 1;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        if (file != NULL) {
            fclose(file);
        }
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    file = fopen(path, "rb");
    if (file == NULL) {
        osr_set_error(err, err_len, "failed to open JPEG file");
        return 0;
    }
    if (offset > (unsigned long long)LONG_MAX || fseek(file, (long)offset, SEEK_SET) != 0) {
        osr_set_error(err, err_len, "failed to seek JPEG file");
        fclose(file);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_stdio_src(&cinfo, file);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;
    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > cinfo.output_width ||
        (unsigned long)y + (unsigned long)h > cinfo.output_height ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "JPEG RGB crop rectangle/components are invalid");
        goto rgb_file_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto rgb_file_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto rgb_file_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    for (unsigned int row = 0; row < h; row++) {
        if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
            osr_set_error(err, err_len, "JPEG RGB crop scanline read failed");
            goto rgb_file_finish;
        }
        memcpy(out + (size_t)row * w * 3,
               rows[0] + (size_t)left * cinfo.output_components,
               (size_t)w * 3);
    }

    result = 1;

rgb_file_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    fclose(file);
    return result;
}

int osr_jpeg_file_sampled_rgb(const char *path,
                              unsigned long long offset,
                              unsigned int x,
                              unsigned int y,
                              unsigned int w,
                              unsigned int h,
                              double sample_x0,
                              double sample_y0,
                              double sample_step,
                              unsigned int out_w,
                              unsigned int out_h,
                              int use_libjpeg_scale,
                              unsigned char *out,
                              char *err,
                              size_t err_len) {
    struct jpeg_decompress_struct cinfo;
    struct osr_jpeg_error jerr;
    JSAMPARRAY rows = NULL;
    FILE *file = NULL;
    int result = 0;

    if (path == NULL || out == NULL) {
        osr_set_error(err, err_len, "invalid null JPEG sampled RGB argument");
        return 0;
    }
    if (w == 0 || h == 0 || out_w == 0 || out_h == 0) {
        return 1;
    }
    if (sample_step <= 0.0) {
        osr_set_error(err, err_len, "invalid JPEG sampled RGB step");
        return 0;
    }

    memset(&cinfo, 0, sizeof(cinfo));
    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = osr_jpeg_error_exit;

    if (setjmp(jerr.setjmp_buffer)) {
        osr_set_error(err, err_len, jerr.message);
        if (file != NULL) {
            fclose(file);
        }
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    file = fopen(path, "rb");
    if (file == NULL) {
        osr_set_error(err, err_len, "failed to open JPEG file");
        return 0;
    }
    if (offset > (unsigned long long)LONG_MAX || fseek(file, (long)offset, SEEK_SET) != 0) {
        osr_set_error(err, err_len, "failed to seek JPEG file");
        fclose(file);
        return 0;
    }

    jpeg_create_decompress(&cinfo);
    jpeg_stdio_src(&cinfo, file);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_EXT_RGB;

    unsigned int image_width = cinfo.image_width;
    unsigned int image_height = cinfo.image_height;
    int scale_denom = use_libjpeg_scale ? osr_jpeg_scale_denom(sample_step) : 0;
    long scaled_x0 = 0;
    long scaled_y0 = 0;
    if (scale_denom != 0) {
        long src_x0 = (long)x + osr_floor_to_long(sample_x0);
        long src_y0 = (long)y + osr_floor_to_long(sample_y0);
        if (src_x0 >= 0 && src_y0 >= 0) {
            scaled_x0 = src_x0 / scale_denom;
            scaled_y0 = src_y0 / scale_denom;
            cinfo.scale_num = 1;
            cinfo.scale_denom = scale_denom;
        } else {
            scale_denom = 0;
        }
    }

    jpeg_start_decompress(&cinfo);

    if ((unsigned long)x + (unsigned long)w > image_width ||
        (unsigned long)y + (unsigned long)h > image_height ||
        cinfo.output_components != 3) {
        osr_set_error(err, err_len, "JPEG sampled RGB rectangle/components are invalid");
        goto sampled_finish;
    }

    if (scale_denom != 0 &&
        (unsigned long)scaled_x0 + (unsigned long)out_w <= cinfo.output_width &&
        (unsigned long)scaled_y0 + (unsigned long)out_h <= cinfo.output_height) {
        JDIMENSION scaled_crop_x = (JDIMENSION)scaled_x0;
        JDIMENSION scaled_crop_w = (JDIMENSION)out_w;
        jpeg_crop_scanline(&cinfo, &scaled_crop_x, &scaled_crop_w);
        if (scaled_crop_x > (JDIMENSION)scaled_x0 ||
            scaled_crop_x + scaled_crop_w < (JDIMENSION)scaled_x0 + (JDIMENSION)out_w) {
            osr_set_error(err, err_len, "libjpeg returned an invalid scaled crop rectangle");
            goto sampled_finish;
        }

        if (scaled_y0 > 0) {
            JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)scaled_y0);
            if (skipped != (JDIMENSION)scaled_y0) {
                osr_set_error(err, err_len, "JPEG scaled sampled RGB vertical skip failed");
                goto sampled_finish;
            }
        }

        rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                          JPOOL_IMAGE,
                                          cinfo.output_width * cinfo.output_components,
                                          1);
        unsigned int scaled_left = (unsigned int)((JDIMENSION)scaled_x0 - scaled_crop_x);
        for (unsigned int out_y = 0; out_y < out_h; out_y++) {
            if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
                osr_set_error(err, err_len, "JPEG scaled sampled RGB scanline read failed");
                goto sampled_finish;
            }
            unsigned char *dst = out + (size_t)out_y * out_w * 3;
            memcpy(dst,
                   rows[0] + (size_t)scaled_left * cinfo.output_components,
                   (size_t)out_w * 3);
        }
        result = 1;
        goto sampled_finish;
    }

    JDIMENSION crop_x = (JDIMENSION)x;
    JDIMENSION crop_w = (JDIMENSION)w;
    jpeg_crop_scanline(&cinfo, &crop_x, &crop_w);
    if (crop_x > x || crop_x + crop_w < x + w) {
        osr_set_error(err, err_len, "libjpeg returned an invalid crop rectangle");
        goto sampled_finish;
    }

    if (y > 0) {
        JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, (JDIMENSION)y);
        if (skipped != y) {
            osr_set_error(err, err_len, "JPEG vertical crop skip failed");
            goto sampled_finish;
        }
    }

    rows = (*cinfo.mem->alloc_sarray)((j_common_ptr)&cinfo,
                                      JPOOL_IMAGE,
                                      cinfo.output_width * cinfo.output_components,
                                      1);
    unsigned int left = x - crop_x;
    unsigned int current_src_y = 0;
    long loaded_src_y = -1;

    for (unsigned int out_y = 0; out_y < out_h; out_y++) {
        double src_y_f = sample_y0 + (double)out_y * sample_step;
        long target_src_y = osr_floor_to_long(src_y_f);
        if (target_src_y < 0) {
            target_src_y = 0;
        } else if ((unsigned long)target_src_y >= h) {
            target_src_y = (long)h - 1;
        }

        if (target_src_y != loaded_src_y) {
            if ((unsigned long)target_src_y < current_src_y) {
                osr_set_error(err, err_len, "JPEG sampled RGB source rows are not monotonic");
                goto sampled_finish;
            }
            JDIMENSION rows_to_skip = (JDIMENSION)((unsigned long)target_src_y - current_src_y);
            if (rows_to_skip > 0) {
                JDIMENSION skipped = jpeg_skip_scanlines(&cinfo, rows_to_skip);
                if (skipped != rows_to_skip) {
                    osr_set_error(err, err_len, "JPEG sampled RGB skip failed");
                    goto sampled_finish;
                }
                current_src_y += skipped;
            }
            if (jpeg_read_scanlines(&cinfo, rows, 1) != 1) {
                osr_set_error(err, err_len, "JPEG sampled RGB scanline read failed");
                goto sampled_finish;
            }
            loaded_src_y = target_src_y;
            current_src_y++;
        }

        unsigned char *dst = out + (size_t)out_y * out_w * 3;
        for (unsigned int out_x = 0; out_x < out_w; out_x++) {
            double src_x_f = sample_x0 + (double)out_x * sample_step;
            long src_x = osr_floor_to_long(src_x_f);
            if (src_x < 0) {
                src_x = 0;
            } else if ((unsigned long)src_x >= w) {
                src_x = (long)w - 1;
            }
            unsigned char *src = rows[0] + ((size_t)left + (size_t)src_x) * cinfo.output_components;
            dst[(size_t)out_x * 3 + 0] = src[0];
            dst[(size_t)out_x * 3 + 1] = src[1];
            dst[(size_t)out_x * 3 + 2] = src[2];
        }
    }

    result = 1;

sampled_finish:
    jpeg_abort_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    fclose(file);
    return result;
}
