#include <cairo.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static void osr_cairo_set_error(char *err, size_t err_len, const char *message) {
    if (err == NULL || err_len == 0) {
        return;
    }
    snprintf(err, err_len, "%s", message);
}

static unsigned char osr_channel_value(const unsigned char *rgb,
                                       int channel) {
    if (channel < 0) {
        return 0;
    }
    if (channel > 2) {
        channel = 2;
    }
    return rgb[channel];
}

static unsigned char osr_unpremultiply(unsigned char value, unsigned char alpha) {
    if (alpha == 0 || alpha == 255) {
        return value;
    }
    int unpremultiplied = ((int)value * 255) / alpha;
    return unpremultiplied > 255 ? 255 : (unsigned char)unpremultiplied;
}

static unsigned char osr_premultiply(unsigned char value, unsigned char alpha) {
    if (alpha == 0 || alpha == 255) {
        return alpha == 0 ? 0 : value;
    }
    return (unsigned char)(((int)value * alpha + 127) / 255);
}

static int osr_cairo_blit_rgb_to_rgba_with_operator(const unsigned char *src_rgb,
                                                    unsigned int src_width,
                                                    unsigned int src_height,
                                                    unsigned int valid_width,
                                                    unsigned int valid_height,
                                                    double src_x,
                                                    double src_y,
                                                    unsigned int src_w,
                                                    unsigned int src_h,
                                                    int channel_r,
                                                    int channel_g,
                                                    int channel_b,
                                                    int channel_a,
                                                    unsigned char *dst_rgba,
                                                    unsigned int dst_width,
                                                    unsigned int dst_height,
                                                    double dst_x,
                                                    double dst_y,
                                                    cairo_operator_t op,
                                                    char *err,
                                                    size_t err_len) {
    if (src_rgb == NULL || dst_rgba == NULL) {
        osr_cairo_set_error(err, err_len, "invalid null Cairo blit argument");
        return 0;
    }
    if (src_w == 0 || src_h == 0 || dst_width == 0 || dst_height == 0) {
        return 1;
    }
    if (src_width > INT32_MAX / 4 || dst_width > INT32_MAX / 4 ||
        src_height > INT32_MAX || dst_height > INT32_MAX) {
        osr_cairo_set_error(err, err_len, "Cairo blit dimensions are too large");
        return 0;
    }
    if (valid_width > src_width) {
        valid_width = src_width;
    }
    if (valid_height > src_height) {
        valid_height = src_height;
    }

    int use_subtile = src_x != 0.0 || src_y != 0.0 ||
                      src_w != src_width || src_h != src_height;
    unsigned int copy_x = 0;
    unsigned int copy_y = 0;
    unsigned int copy_w = src_width;
    unsigned int copy_h = src_height;
    if (use_subtile) {
        double src_right = src_x + src_w;
        double src_bottom = src_y + src_h;
        int start_x = (int)floor(src_x) - 1;
        int start_y = (int)floor(src_y) - 1;
        int end_x = (int)ceil(src_right) + 1;
        int end_y = (int)ceil(src_bottom) + 1;
        if (start_x < 0) {
            start_x = 0;
        }
        if (start_y < 0) {
            start_y = 0;
        }
        if (end_x > (int)src_width) {
            end_x = (int)src_width;
        }
        if (end_y > (int)src_height) {
            end_y = (int)src_height;
        }
        if (end_x <= start_x || end_y <= start_y) {
            return 1;
        }
        copy_x = (unsigned int)start_x;
        copy_y = (unsigned int)start_y;
        copy_w = (unsigned int)(end_x - start_x);
        copy_h = (unsigned int)(end_y - start_y);
    }

    size_t src_stride = (size_t)copy_w * 4;
    size_t dst_stride = (size_t)dst_width * 4;
    size_t src_len = src_stride * copy_h;
    size_t dst_len = dst_stride * dst_height;
    unsigned char *src_argb = (unsigned char *)malloc(src_len);
    unsigned char *dst_argb = (unsigned char *)malloc(dst_len);
    if (src_argb == NULL || dst_argb == NULL) {
        free(src_argb);
        free(dst_argb);
        osr_cairo_set_error(err, err_len, "failed to allocate Cairo blit buffers");
        return 0;
    }

    for (unsigned int row = 0; row < dst_height; row++) {
        for (unsigned int col = 0; col < dst_width; col++) {
            const unsigned char *src = dst_rgba + ((size_t)row * dst_width + col) * 4;
            unsigned char *dst = dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
    }

    for (unsigned int row = 0; row < copy_h; row++) {
        for (unsigned int col = 0; col < copy_w; col++) {
            const unsigned char *src =
                src_rgb + ((size_t)(copy_y + row) * src_width + (copy_x + col)) * 3;
            unsigned char *dst = src_argb + (size_t)row * src_stride + (size_t)col * 4;
            dst[0] = osr_channel_value(src, channel_b);
            dst[1] = osr_channel_value(src, channel_g);
            dst[2] = osr_channel_value(src, channel_r);
            if (copy_x + col >= valid_width || copy_y + row >= valid_height) {
                dst[0] = 0;
                dst[1] = 0;
                dst[2] = 0;
                dst[3] = 0;
            } else {
                dst[3] = channel_a < 0 ? 255 : osr_channel_value(src, channel_a);
            }
        }
    }

    cairo_surface_t *dst_surface =
        cairo_image_surface_create_for_data(dst_argb, CAIRO_FORMAT_ARGB32,
                                            (int)dst_width, (int)dst_height,
                                            (int)dst_stride);
    cairo_surface_t *src_surface =
        cairo_image_surface_create_for_data(src_argb, CAIRO_FORMAT_ARGB32,
                                            (int)copy_w, (int)copy_h,
                                            (int)src_stride);
    unsigned char *subtile_argb = NULL;
    cairo_surface_t *subtile_surface = NULL;
    cairo_t *cr = cairo_create(dst_surface);

    cairo_status_t status = cairo_status(cr);
    if (status == CAIRO_STATUS_SUCCESS) {
        if (use_subtile) {
            size_t subtile_stride = (size_t)src_w * 4;
            size_t subtile_len = subtile_stride * src_h;
            subtile_argb = (unsigned char *)calloc(1, subtile_len);
            if (subtile_argb == NULL) {
                status = CAIRO_STATUS_NO_MEMORY;
            } else {
                subtile_surface =
                    cairo_image_surface_create_for_data(subtile_argb, CAIRO_FORMAT_ARGB32,
                                                        (int)src_w, (int)src_h,
                                                        (int)subtile_stride);
                cairo_t *cr2 = cairo_create(subtile_surface);
                status = cairo_status(cr2);
                if (status == CAIRO_STATUS_SUCCESS) {
                    cairo_set_source_surface(cr2, src_surface,
                                             (double)copy_x - src_x,
                                             (double)copy_y - src_y);
                    cairo_rectangle(cr2, 0, 0, src_w, src_h);
                    cairo_fill(cr2);
                    status = cairo_status(cr2);
                }
                cairo_destroy(cr2);
                cairo_surface_flush(subtile_surface);
            }
        }
    }
    if (status == CAIRO_STATUS_SUCCESS) {
        cairo_set_operator(cr, op);
        cairo_set_source_surface(cr,
                                 subtile_surface != NULL ? subtile_surface : src_surface,
                                 dst_x,
                                 dst_y);
        cairo_paint(cr);
        status = cairo_status(cr);
    }
    cairo_surface_flush(dst_surface);

    int ok = status == CAIRO_STATUS_SUCCESS;
    if (ok) {
        for (unsigned int row = 0; row < dst_height; row++) {
            for (unsigned int col = 0; col < dst_width; col++) {
                const unsigned char *src = dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
                unsigned char *dst = dst_rgba + ((size_t)row * dst_width + col) * 4;
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
    } else {
        osr_cairo_set_error(err, err_len, cairo_status_to_string(status));
    }

    cairo_destroy(cr);
    if (subtile_surface != NULL) {
        cairo_surface_destroy(subtile_surface);
    }
    cairo_surface_destroy(src_surface);
    cairo_surface_destroy(dst_surface);
    free(subtile_argb);
    free(src_argb);
    free(dst_argb);
    return ok;
}

int osr_cairo_blit_rgb_to_rgba(const unsigned char *src_rgb,
                               unsigned int src_width,
                               unsigned int src_height,
                               unsigned int valid_width,
                               unsigned int valid_height,
                               double src_x,
                               double src_y,
                               unsigned int src_w,
                               unsigned int src_h,
                               int channel_r,
                               int channel_g,
                               int channel_b,
                               int channel_a,
                               unsigned char *dst_rgba,
                               unsigned int dst_width,
                               unsigned int dst_height,
                               double dst_x,
                               double dst_y,
                               char *err,
                               size_t err_len) {
    return osr_cairo_blit_rgb_to_rgba_with_operator(
        src_rgb, src_width, src_height, valid_width, valid_height,
        src_x, src_y, src_w, src_h, channel_r, channel_g, channel_b, channel_a,
        dst_rgba, dst_width, dst_height, dst_x, dst_y, CAIRO_OPERATOR_SATURATE,
        err, err_len);
}

int osr_cairo_blit_rgb_to_rgba_clipped_dst(const unsigned char *src_rgb,
                                           unsigned int src_width,
                                           unsigned int src_height,
                                           unsigned int valid_width,
                                           unsigned int valid_height,
                                           double src_x,
                                           double src_y,
                                           unsigned int src_w,
                                           unsigned int src_h,
                                           int channel_r,
                                           int channel_g,
                                           int channel_b,
                                           int channel_a,
                                           unsigned char *dst_rgba,
                                           unsigned int dst_width,
                                           unsigned int dst_height,
                                           double dst_x,
                                           double dst_y,
                                           char *err,
                                           size_t err_len) {
    if (src_rgb == NULL || dst_rgba == NULL) {
        osr_cairo_set_error(err, err_len, "invalid null clipped Cairo blit argument");
        return 0;
    }
    if (src_w == 0 || src_h == 0 || dst_width == 0 || dst_height == 0) {
        return 1;
    }
    if (src_width > INT32_MAX / 4 || dst_width > INT32_MAX / 4 ||
        src_height > INT32_MAX || dst_height > INT32_MAX) {
        osr_cairo_set_error(err, err_len, "clipped Cairo blit dimensions are too large");
        return 0;
    }
    if (valid_width > src_width) {
        valid_width = src_width;
    }
    if (valid_height > src_height) {
        valid_height = src_height;
    }

    int dst_start_x = (int)floor(dst_x) - 1;
    int dst_start_y = (int)floor(dst_y) - 1;
    int dst_end_x = (int)ceil(dst_x + src_w) + 1;
    int dst_end_y = (int)ceil(dst_y + src_h) + 1;
    if (dst_start_x < 0) {
        dst_start_x = 0;
    }
    if (dst_start_y < 0) {
        dst_start_y = 0;
    }
    if (dst_end_x > (int)dst_width) {
        dst_end_x = (int)dst_width;
    }
    if (dst_end_y > (int)dst_height) {
        dst_end_y = (int)dst_height;
    }
    if (dst_end_x <= dst_start_x || dst_end_y <= dst_start_y) {
        return 1;
    }

    unsigned int clip_w = (unsigned int)(dst_end_x - dst_start_x);
    unsigned int clip_h = (unsigned int)(dst_end_y - dst_start_y);
    size_t dst_stride = (size_t)clip_w * 4;
    size_t dst_len = dst_stride * clip_h;
    unsigned char *dst_argb = (unsigned char *)malloc(dst_len);
    if (dst_argb == NULL) {
        osr_cairo_set_error(err, err_len, "failed to allocate clipped Cairo destination buffer");
        return 0;
    }

    for (unsigned int row = 0; row < clip_h; row++) {
        for (unsigned int col = 0; col < clip_w; col++) {
            const unsigned char *src =
                dst_rgba + ((size_t)(dst_start_y + (int)row) * dst_width +
                            (dst_start_x + (int)col)) * 4;
            unsigned char *dst = dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
    }

    int use_subtile = src_x != 0.0 || src_y != 0.0 ||
                      src_w != src_width || src_h != src_height;
    unsigned int copy_x = 0;
    unsigned int copy_y = 0;
    unsigned int copy_w = src_width;
    unsigned int copy_h = src_height;
    if (use_subtile) {
        double src_right = src_x + src_w;
        double src_bottom = src_y + src_h;
        int start_x = (int)floor(src_x) - 1;
        int start_y = (int)floor(src_y) - 1;
        int end_x = (int)ceil(src_right) + 1;
        int end_y = (int)ceil(src_bottom) + 1;
        if (start_x < 0) {
            start_x = 0;
        }
        if (start_y < 0) {
            start_y = 0;
        }
        if (end_x > (int)src_width) {
            end_x = (int)src_width;
        }
        if (end_y > (int)src_height) {
            end_y = (int)src_height;
        }
        if (end_x <= start_x || end_y <= start_y) {
            free(dst_argb);
            return 1;
        }
        copy_x = (unsigned int)start_x;
        copy_y = (unsigned int)start_y;
        copy_w = (unsigned int)(end_x - start_x);
        copy_h = (unsigned int)(end_y - start_y);
    } else {
        int start_x = (int)floor((double)dst_start_x - dst_x) - 1;
        int start_y = (int)floor((double)dst_start_y - dst_y) - 1;
        int end_x = (int)ceil((double)dst_end_x - dst_x) + 1;
        int end_y = (int)ceil((double)dst_end_y - dst_y) + 1;
        if (start_x < 0) {
            start_x = 0;
        }
        if (start_y < 0) {
            start_y = 0;
        }
        if (end_x > (int)src_width) {
            end_x = (int)src_width;
        }
        if (end_y > (int)src_height) {
            end_y = (int)src_height;
        }
        if (end_x <= start_x || end_y <= start_y) {
            free(dst_argb);
            return 1;
        }
        copy_x = (unsigned int)start_x;
        copy_y = (unsigned int)start_y;
        copy_w = (unsigned int)(end_x - start_x);
        copy_h = (unsigned int)(end_y - start_y);
    }

    size_t src_stride = (size_t)copy_w * 4;
    size_t src_len = src_stride * copy_h;
    unsigned char *src_argb = (unsigned char *)malloc(src_len);
    if (src_argb == NULL) {
        free(dst_argb);
        osr_cairo_set_error(err, err_len, "failed to allocate clipped Cairo source buffer");
        return 0;
    }

    for (unsigned int row = 0; row < copy_h; row++) {
        for (unsigned int col = 0; col < copy_w; col++) {
            const unsigned char *src =
                src_rgb + ((size_t)(copy_y + row) * src_width + (copy_x + col)) * 3;
            unsigned char *dst = src_argb + (size_t)row * src_stride + (size_t)col * 4;
            dst[0] = osr_channel_value(src, channel_b);
            dst[1] = osr_channel_value(src, channel_g);
            dst[2] = osr_channel_value(src, channel_r);
            if (copy_x + col >= valid_width || copy_y + row >= valid_height) {
                dst[0] = 0;
                dst[1] = 0;
                dst[2] = 0;
                dst[3] = 0;
            } else {
                dst[3] = channel_a < 0 ? 255 : osr_channel_value(src, channel_a);
            }
        }
    }

    cairo_surface_t *dst_surface =
        cairo_image_surface_create_for_data(dst_argb, CAIRO_FORMAT_ARGB32,
                                            (int)clip_w, (int)clip_h,
                                            (int)dst_stride);
    cairo_surface_t *src_surface =
        cairo_image_surface_create_for_data(src_argb, CAIRO_FORMAT_ARGB32,
                                            (int)copy_w, (int)copy_h,
                                            (int)src_stride);
    unsigned char *subtile_argb = NULL;
    cairo_surface_t *subtile_surface = NULL;
    cairo_t *cr = cairo_create(dst_surface);

    cairo_status_t status = cairo_status(cr);
    if (status == CAIRO_STATUS_SUCCESS && use_subtile) {
        size_t subtile_stride = (size_t)src_w * 4;
        size_t subtile_len = subtile_stride * src_h;
        subtile_argb = (unsigned char *)calloc(1, subtile_len);
        if (subtile_argb == NULL) {
            status = CAIRO_STATUS_NO_MEMORY;
        } else {
            subtile_surface =
                cairo_image_surface_create_for_data(subtile_argb, CAIRO_FORMAT_ARGB32,
                                                    (int)src_w, (int)src_h,
                                                    (int)subtile_stride);
            cairo_t *cr2 = cairo_create(subtile_surface);
            status = cairo_status(cr2);
            if (status == CAIRO_STATUS_SUCCESS) {
                cairo_set_source_surface(cr2, src_surface,
                                         (double)copy_x - src_x,
                                         (double)copy_y - src_y);
                cairo_rectangle(cr2, 0, 0, src_w, src_h);
                cairo_fill(cr2);
                status = cairo_status(cr2);
            }
            cairo_destroy(cr2);
            cairo_surface_flush(subtile_surface);
        }
    }
    if (status == CAIRO_STATUS_SUCCESS) {
        cairo_set_operator(cr, CAIRO_OPERATOR_SATURATE);
        cairo_set_source_surface(cr,
                                 subtile_surface != NULL ? subtile_surface : src_surface,
                                 dst_x + (use_subtile ? 0.0 : copy_x) - dst_start_x,
                                 dst_y + (use_subtile ? 0.0 : copy_y) - dst_start_y);
        cairo_paint(cr);
        status = cairo_status(cr);
    }
    cairo_surface_flush(dst_surface);

    int ok = status == CAIRO_STATUS_SUCCESS;
    if (ok) {
        for (unsigned int row = 0; row < clip_h; row++) {
            for (unsigned int col = 0; col < clip_w; col++) {
                const unsigned char *src =
                    dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
                unsigned char *dst =
                    dst_rgba + ((size_t)(dst_start_y + (int)row) * dst_width +
                                (dst_start_x + (int)col)) * 4;
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
    } else {
        osr_cairo_set_error(err, err_len, cairo_status_to_string(status));
    }

    cairo_destroy(cr);
    if (subtile_surface != NULL) {
        cairo_surface_destroy(subtile_surface);
    }
    cairo_surface_destroy(src_surface);
    cairo_surface_destroy(dst_surface);
    free(subtile_argb);
    free(src_argb);
    free(dst_argb);
    return ok;
}

int osr_cairo_blit_rgb_to_rgba_many_same_src(const unsigned char *src_rgb,
                                             unsigned int src_width,
                                             unsigned int src_height,
                                             unsigned int valid_width,
                                             unsigned int valid_height,
                                             const double *src_xs,
                                             const double *src_ys,
                                             unsigned int src_w,
                                             unsigned int src_h,
                                             int channel_r,
                                             int channel_g,
                                             int channel_b,
                                             int channel_a,
                                             unsigned char *dst_rgba,
                                             unsigned int dst_width,
                                             unsigned int dst_height,
                                             const double *dst_xs,
                                             const double *dst_ys,
                                             size_t count,
                                             char *err,
                                             size_t err_len) {
    if (src_rgb == NULL || dst_rgba == NULL || src_xs == NULL || src_ys == NULL ||
        dst_xs == NULL || dst_ys == NULL) {
        osr_cairo_set_error(err, err_len, "invalid null Cairo batch blit argument");
        return 0;
    }
    if (count == 0 || src_w == 0 || src_h == 0 || dst_width == 0 || dst_height == 0) {
        return 1;
    }
    if (src_width > INT32_MAX / 4 || dst_width > INT32_MAX / 4 ||
        src_height > INT32_MAX || dst_height > INT32_MAX) {
        osr_cairo_set_error(err, err_len, "Cairo batch blit dimensions are too large");
        return 0;
    }
    if (valid_width > src_width) {
        valid_width = src_width;
    }
    if (valid_height > src_height) {
        valid_height = src_height;
    }

    int dst_start_x = (int)dst_width;
    int dst_start_y = (int)dst_height;
    int dst_end_x = 0;
    int dst_end_y = 0;
    for (size_t i = 0; i < count; i++) {
        int start_x = (int)floor(dst_xs[i]) - 1;
        int start_y = (int)floor(dst_ys[i]) - 1;
        int end_x = (int)ceil(dst_xs[i] + src_w) + 1;
        int end_y = (int)ceil(dst_ys[i] + src_h) + 1;
        if (start_x < 0) {
            start_x = 0;
        }
        if (start_y < 0) {
            start_y = 0;
        }
        if (end_x > (int)dst_width) {
            end_x = (int)dst_width;
        }
        if (end_y > (int)dst_height) {
            end_y = (int)dst_height;
        }
        if (end_x <= start_x || end_y <= start_y) {
            continue;
        }
        if (start_x < dst_start_x) {
            dst_start_x = start_x;
        }
        if (start_y < dst_start_y) {
            dst_start_y = start_y;
        }
        if (end_x > dst_end_x) {
            dst_end_x = end_x;
        }
        if (end_y > dst_end_y) {
            dst_end_y = end_y;
        }
    }
    if (dst_end_x <= dst_start_x || dst_end_y <= dst_start_y) {
        return 1;
    }

    unsigned int clip_w = (unsigned int)(dst_end_x - dst_start_x);
    unsigned int clip_h = (unsigned int)(dst_end_y - dst_start_y);
    size_t dst_stride = (size_t)clip_w * 4;
    size_t dst_len = dst_stride * clip_h;
    unsigned char *dst_argb = (unsigned char *)malloc(dst_len);
    if (dst_argb == NULL) {
        osr_cairo_set_error(err, err_len, "failed to allocate Cairo batch destination buffer");
        return 0;
    }

    for (unsigned int row = 0; row < clip_h; row++) {
        for (unsigned int col = 0; col < clip_w; col++) {
            const unsigned char *src =
                dst_rgba + ((size_t)(dst_start_y + (int)row) * dst_width +
                            (dst_start_x + (int)col)) * 4;
            unsigned char *dst = dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
    }

    cairo_surface_t *dst_surface =
        cairo_image_surface_create_for_data(dst_argb, CAIRO_FORMAT_ARGB32,
                                            (int)clip_w, (int)clip_h,
                                            (int)dst_stride);
    cairo_t *cr = cairo_create(dst_surface);
    cairo_status_t status = cairo_status(cr);
    if (status == CAIRO_STATUS_SUCCESS) {
        cairo_set_operator(cr, CAIRO_OPERATOR_SATURATE);
    }

    for (size_t i = 0; i < count && status == CAIRO_STATUS_SUCCESS; i++) {
        double src_x = src_xs[i];
        double src_y = src_ys[i];
        double src_right = src_x + src_w;
        double src_bottom = src_y + src_h;
        int start_x = (int)floor(src_x) - 1;
        int start_y = (int)floor(src_y) - 1;
        int end_x = (int)ceil(src_right) + 1;
        int end_y = (int)ceil(src_bottom) + 1;
        if (start_x < 0) {
            start_x = 0;
        }
        if (start_y < 0) {
            start_y = 0;
        }
        if (end_x > (int)src_width) {
            end_x = (int)src_width;
        }
        if (end_y > (int)src_height) {
            end_y = (int)src_height;
        }
        if (end_x <= start_x || end_y <= start_y) {
            continue;
        }

        unsigned int copy_x = (unsigned int)start_x;
        unsigned int copy_y = (unsigned int)start_y;
        unsigned int copy_w = (unsigned int)(end_x - start_x);
        unsigned int copy_h = (unsigned int)(end_y - start_y);
        size_t src_stride = (size_t)copy_w * 4;
        size_t src_len = src_stride * copy_h;
        unsigned char *src_argb = (unsigned char *)malloc(src_len);
        if (src_argb == NULL) {
            status = CAIRO_STATUS_NO_MEMORY;
            break;
        }

        for (unsigned int row = 0; row < copy_h; row++) {
            for (unsigned int col = 0; col < copy_w; col++) {
                const unsigned char *src =
                    src_rgb + ((size_t)(copy_y + row) * src_width + (copy_x + col)) * 3;
                unsigned char *dst = src_argb + (size_t)row * src_stride + (size_t)col * 4;
                dst[0] = osr_channel_value(src, channel_b);
                dst[1] = osr_channel_value(src, channel_g);
                dst[2] = osr_channel_value(src, channel_r);
                if (copy_x + col >= valid_width || copy_y + row >= valid_height) {
                    dst[0] = 0;
                    dst[1] = 0;
                    dst[2] = 0;
                    dst[3] = 0;
                } else {
                    dst[3] = channel_a < 0 ? 255 : osr_channel_value(src, channel_a);
                }
            }
        }

        cairo_surface_t *src_surface =
            cairo_image_surface_create_for_data(src_argb, CAIRO_FORMAT_ARGB32,
                                                (int)copy_w, (int)copy_h,
                                                (int)src_stride);
        size_t subtile_stride = (size_t)src_w * 4;
        size_t subtile_len = subtile_stride * src_h;
        unsigned char *subtile_argb = (unsigned char *)calloc(1, subtile_len);
        cairo_surface_t *subtile_surface = NULL;
        if (subtile_argb == NULL) {
            status = CAIRO_STATUS_NO_MEMORY;
        } else {
            subtile_surface =
                cairo_image_surface_create_for_data(subtile_argb, CAIRO_FORMAT_ARGB32,
                                                    (int)src_w, (int)src_h,
                                                    (int)subtile_stride);
            cairo_t *cr2 = cairo_create(subtile_surface);
            status = cairo_status(cr2);
            if (status == CAIRO_STATUS_SUCCESS) {
                cairo_set_source_surface(cr2, src_surface,
                                         (double)copy_x - src_x,
                                         (double)copy_y - src_y);
                cairo_rectangle(cr2, 0, 0, src_w, src_h);
                cairo_fill(cr2);
                status = cairo_status(cr2);
            }
            cairo_destroy(cr2);
            cairo_surface_flush(subtile_surface);
        }
        if (status == CAIRO_STATUS_SUCCESS) {
            cairo_set_source_surface(cr, subtile_surface,
                                     dst_xs[i] - dst_start_x,
                                     dst_ys[i] - dst_start_y);
            cairo_paint(cr);
            status = cairo_status(cr);
        }
        if (subtile_surface != NULL) {
            cairo_surface_destroy(subtile_surface);
        }
        cairo_surface_destroy(src_surface);
        free(subtile_argb);
        free(src_argb);
    }

    cairo_surface_flush(dst_surface);
    int ok = status == CAIRO_STATUS_SUCCESS;
    if (ok) {
        for (unsigned int row = 0; row < clip_h; row++) {
            for (unsigned int col = 0; col < clip_w; col++) {
                const unsigned char *src = dst_argb + (size_t)row * dst_stride + (size_t)col * 4;
                unsigned char *dst =
                    dst_rgba + ((size_t)(dst_start_y + (int)row) * dst_width +
                                (dst_start_x + (int)col)) * 4;
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
    } else {
        osr_cairo_set_error(err, err_len, cairo_status_to_string(status));
    }

    cairo_destroy(cr);
    cairo_surface_destroy(dst_surface);
    free(dst_argb);
    return ok;
}
