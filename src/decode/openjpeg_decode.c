#include <math.h>
#include <openjpeg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct osr_jp2k_buffer {
    const unsigned char *data;
    OPJ_SIZE_T offset;
    OPJ_SIZE_T length;
};

static void osr_jp2k_set_error(char *err, size_t err_len, const char *message) {
    if (err == NULL || err_len == 0) {
        return;
    }
    snprintf(err, err_len, "%s", message);
}

static OPJ_SIZE_T osr_jp2k_read(void *buf, OPJ_SIZE_T count, void *data) {
    struct osr_jp2k_buffer *state = (struct osr_jp2k_buffer *)data;
    OPJ_SIZE_T remaining = state->length - state->offset;
    if (count > remaining) {
        count = remaining;
    }
    if (count == 0) {
        return (OPJ_SIZE_T)-1;
    }
    memcpy(buf, state->data + state->offset, count);
    state->offset += count;
    return count;
}

static OPJ_OFF_T osr_jp2k_skip(OPJ_OFF_T count, void *data) {
    struct osr_jp2k_buffer *state = (struct osr_jp2k_buffer *)data;
    OPJ_SIZE_T old = state->offset;
    if (count < 0) {
        OPJ_SIZE_T back = (OPJ_SIZE_T)(-count);
        state->offset = back > state->offset ? 0 : state->offset - back;
    } else {
        OPJ_SIZE_T forward = (OPJ_SIZE_T)count;
        state->offset = forward > state->length - state->offset
                            ? state->length
                            : state->offset + forward;
    }
    if (count != 0 && state->offset == old) {
        return -1;
    }
    return (OPJ_OFF_T)state->offset - (OPJ_OFF_T)old;
}

static OPJ_BOOL osr_jp2k_seek(OPJ_OFF_T offset, void *data) {
    struct osr_jp2k_buffer *state = (struct osr_jp2k_buffer *)data;
    if (offset < 0 || (OPJ_SIZE_T)offset > state->length) {
        return OPJ_FALSE;
    }
    state->offset = (OPJ_SIZE_T)offset;
    return OPJ_TRUE;
}

static unsigned char osr_clamp_i32(int value) {
    if (value < 0) {
        return 0;
    }
    if (value > 255) {
        return 255;
    }
    return (unsigned char)value;
}

static int osr_r_cr(int value) {
    return (int)round(1.402 * (double)(value - 128));
}

static int osr_g_cb(int value) {
    return (int)round((double)(1 << 16) * (0.5 - 0.34414 * (double)(value - 128)));
}

static int osr_g_cr(int value) {
    return (int)round((double)(1 << 16) * -0.71414 * (double)(value - 128));
}

static int osr_b_cb(int value) {
    return (int)round(1.772 * (double)(value - 128));
}

static void osr_write_ycbcr(unsigned char *dst, int y, int cb, int cr) {
    int r_chroma = osr_r_cr(cr);
    int g_chroma = (osr_g_cb(cb) + osr_g_cr(cr)) >> 16;
    int b_chroma = osr_b_cb(cb);
    dst[0] = osr_clamp_i32(y + r_chroma);
    dst[1] = osr_clamp_i32(y + g_chroma);
    dst[2] = osr_clamp_i32(y + b_chroma);
}

static void osr_unpack_aperio_ycbcr(opj_image_comp_t *comps,
                                    unsigned char *out,
                                    unsigned int width,
                                    unsigned int height) {
    for (unsigned int y = 0; y < height; y++) {
        unsigned int y_base = y * comps[0].w;
        unsigned int cb_base = y * comps[1].w;
        unsigned int cr_base = y * comps[2].w;
        unsigned int x = 0;
        for (; x + 1 < width; x += 2) {
            int yy = comps[0].data[y_base + x];
            int cb = comps[1].data[cb_base + x / 2];
            int cr = comps[2].data[cr_base + x / 2];
            osr_write_ycbcr(out + ((size_t)y * width + x) * 3, yy, cb, cr);
            yy = comps[0].data[y_base + x + 1];
            osr_write_ycbcr(out + ((size_t)y * width + x + 1) * 3, yy, cb, cr);
        }
        if (x < width) {
            int yy = comps[0].data[y_base + x];
            int cb = comps[1].data[cb_base + x / 2];
            int cr = comps[2].data[cr_base + x / 2];
            osr_write_ycbcr(out + ((size_t)y * width + x) * 3, yy, cb, cr);
        }
    }
}

static void osr_unpack_rgb(opj_image_comp_t *comps,
                           unsigned char *out,
                           unsigned int width,
                           unsigned int height) {
    unsigned int c0_sub_x = width / comps[0].w;
    unsigned int c1_sub_x = width / comps[1].w;
    unsigned int c2_sub_x = width / comps[2].w;
    unsigned int c0_sub_y = height / comps[0].h;
    unsigned int c1_sub_y = height / comps[1].h;
    unsigned int c2_sub_y = height / comps[2].h;

    for (unsigned int y = 0; y < height; y++) {
        unsigned int c0_base = (y / c0_sub_y) * comps[0].w;
        unsigned int c1_base = (y / c1_sub_y) * comps[1].w;
        unsigned int c2_base = (y / c2_sub_y) * comps[2].w;
        for (unsigned int x = 0; x < width; x++) {
            unsigned char *dst = out + ((size_t)y * width + x) * 3;
            dst[0] = osr_clamp_i32(comps[0].data[c0_base + x / c0_sub_x]);
            dst[1] = osr_clamp_i32(comps[1].data[c1_base + x / c1_sub_x]);
            dst[2] = osr_clamp_i32(comps[2].data[c2_base + x / c2_sub_x]);
        }
    }
}

int osr_openjpeg_decode_rgb(const unsigned char *data,
                            size_t len,
                            unsigned int width,
                            unsigned int height,
                            int ycbcr,
                            unsigned char *out,
                            char *err,
                            size_t err_len) {
    if (data == NULL || out == NULL) {
        osr_jp2k_set_error(err, err_len, "invalid null OpenJPEG decode argument");
        return 0;
    }

    struct osr_jp2k_buffer state = {
        .data = data,
        .offset = 0,
        .length = len,
    };
    opj_stream_t *stream = opj_stream_create(len, OPJ_TRUE);
    opj_codec_t *codec = opj_create_decompress(OPJ_CODEC_J2K);
    opj_image_t *image = NULL;
    int ok = 0;

    if (stream == NULL || codec == NULL) {
        osr_jp2k_set_error(err, err_len, "failed to allocate OpenJPEG decoder");
        goto finish;
    }
    opj_stream_set_user_data(stream, &state, NULL);
    opj_stream_set_user_data_length(stream, len);
    opj_stream_set_read_function(stream, osr_jp2k_read);
    opj_stream_set_skip_function(stream, osr_jp2k_skip);
    opj_stream_set_seek_function(stream, osr_jp2k_seek);

    opj_dparameters_t parameters;
    opj_set_default_decoder_parameters(&parameters);
    if (!opj_setup_decoder(codec, &parameters)) {
        osr_jp2k_set_error(err, err_len, "opj_setup_decoder failed");
        goto finish;
    }
    if (!opj_read_header(stream, codec, &image)) {
        osr_jp2k_set_error(err, err_len, "opj_read_header failed");
        goto finish;
    }
    if (image->x1 != width || image->y1 != height) {
        osr_jp2k_set_error(err, err_len, "OpenJPEG decoded dimensions did not match expected tile");
        goto finish;
    }
    if (image->numcomps != 3) {
        osr_jp2k_set_error(err, err_len, "OpenJPEG decoded tile does not have 3 components");
        goto finish;
    }
    if (!opj_decode(codec, stream, image)) {
        osr_jp2k_set_error(err, err_len, "opj_decode failed");
        goto finish;
    }

    unsigned int c0_sub_x = width / image->comps[0].w;
    unsigned int c1_sub_x = width / image->comps[1].w;
    unsigned int c2_sub_x = width / image->comps[2].w;
    unsigned int c0_sub_y = height / image->comps[0].h;
    unsigned int c1_sub_y = height / image->comps[1].h;
    unsigned int c2_sub_y = height / image->comps[2].h;
    if (ycbcr && c0_sub_x == 1 && c1_sub_x == 2 && c2_sub_x == 2 &&
        c0_sub_y == 1 && c1_sub_y == 1 && c2_sub_y == 1) {
        osr_unpack_aperio_ycbcr(image->comps, out, width, height);
    } else {
        osr_unpack_rgb(image->comps, out, width, height);
    }
    ok = 1;

finish:
    if (image != NULL) {
        opj_image_destroy(image);
    }
    if (codec != NULL) {
        opj_destroy_codec(codec);
    }
    if (stream != NULL) {
        opj_stream_destroy(stream);
    }
    return ok;
}
