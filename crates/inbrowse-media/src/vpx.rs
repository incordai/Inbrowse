//! VP8 / VP9 decoding through libvpx (`vpx` feature).
//!
//! Used by `video.rs::VideoDecoder` for `Codec::Vp8`/`Codec::Vp9`; H.264 goes
//! through moq-video instead. VP8/VP9 never reorder frames, so each output
//! picture takes the presentation time of the packet that produced it.

use std::ffi::CStr;
use std::os::raw::{c_int, c_uint};
use std::ptr;

use vpx_sys::{
    vpx_codec_ctx_t, vpx_codec_dec_cfg_t, vpx_codec_dec_init_ver, vpx_codec_decode,
    vpx_codec_destroy, vpx_codec_err_t, vpx_codec_err_to_string, vpx_codec_get_frame,
    vpx_codec_iter_t, vpx_codec_vp8_dx, vpx_codec_vp9_dx, vpx_img_fmt, VPX_DECODER_ABI_VERSION,
};

use crate::video::Picture;

pub(crate) struct VpxDecoder {
    ctx: vpx_codec_ctx_t,
}

fn describe(err: vpx_codec_err_t) -> String {
    // SAFETY: libvpx returns a pointer to a static, NUL-terminated string.
    unsafe {
        let s = vpx_codec_err_to_string(err);
        if s.is_null() {
            format!("libvpx error {err:?}")
        } else {
            CStr::from_ptr(s).to_string_lossy().into_owned()
        }
    }
}

impl VpxDecoder {
    pub(crate) fn new(vp9: bool) -> Result<Self, String> {
        let mut ctx = vpx_codec_ctx_t::default();
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(1);
        let cfg = vpx_codec_dec_cfg_t {
            threads: threads as c_uint,
            w: 0,
            h: 0,
        };
        // SAFETY: ctx and cfg are valid for the call; the iface getters return
        // static descriptors.
        let err = unsafe {
            let iface = if vp9 {
                vpx_codec_vp9_dx()
            } else {
                vpx_codec_vp8_dx()
            };
            vpx_codec_dec_init_ver(&mut ctx, iface, &cfg, 0, VPX_DECODER_ABI_VERSION as c_int)
        };
        if err != vpx_codec_err_t::VPX_CODEC_OK {
            return Err(format!(
                "could not open the {} decoder: {}",
                if vp9 { "VP9" } else { "VP8" },
                describe(err)
            ));
        }
        Ok(Self { ctx })
    }

    pub(crate) fn decode(&mut self, data: &[u8], pts_us: i64) -> Result<Vec<Picture>, String> {
        let len = c_uint::try_from(data.len()).map_err(|_| "packet too large".to_string())?;
        // SAFETY: data outlives the call; ctx was initialised in new().
        let err =
            unsafe { vpx_codec_decode(&mut self.ctx, data.as_ptr(), len, ptr::null_mut(), 0) };
        if err != vpx_codec_err_t::VPX_CODEC_OK {
            return Err(describe(err));
        }
        self.drain(pts_us)
    }

    pub(crate) fn flush(&mut self, pts_us: i64) -> Result<Vec<Picture>, String> {
        // SAFETY: a null buffer with size 0 signals end of stream.
        let err = unsafe { vpx_codec_decode(&mut self.ctx, ptr::null(), 0, ptr::null_mut(), 0) };
        if err != vpx_codec_err_t::VPX_CODEC_OK {
            return Err(describe(err));
        }
        self.drain(pts_us)
    }

    fn drain(&mut self, pts_us: i64) -> Result<Vec<Picture>, String> {
        let mut out = Vec::new();
        let mut iter: vpx_codec_iter_t = ptr::null();
        loop {
            // SAFETY: iter starts null and is advanced only by libvpx.
            let img = unsafe { vpx_codec_get_frame(&mut self.ctx, &mut iter) };
            if img.is_null() {
                break;
            }
            // SAFETY: libvpx keeps the image valid until the next decode call.
            let img = unsafe { &*img };
            if img.fmt != vpx_img_fmt::VPX_IMG_FMT_I420 {
                return Err(format!(
                    "unsupported pixel format {:?} (only 8-bit 4:2:0)",
                    img.fmt
                ));
            }
            let width = (img.d_w & !1) as usize;
            let height = (img.d_h & !1) as usize;
            if width == 0 || height == 0 {
                continue;
            }
            let mut data = Vec::with_capacity(width * height * 3 / 2);
            for (plane, (w, h)) in [
                (0usize, (width, height)),
                (1, (width / 2, height / 2)),
                (2, (width / 2, height / 2)),
            ] {
                let stride = img.stride[plane];
                if stride < w as c_int || img.planes[plane].is_null() {
                    return Err("libvpx returned an image with a bad plane".into());
                }
                for row in 0..h {
                    // SAFETY: row < plane height and w <= stride, within the
                    // buffer libvpx allocated for this plane.
                    let line = unsafe {
                        std::slice::from_raw_parts(img.planes[plane].add(row * stride as usize), w)
                    };
                    data.extend_from_slice(line);
                }
            }
            out.push(Picture {
                pts_us,
                width: width as u32,
                height: height as u32,
                i420: data,
            });
        }
        Ok(out)
    }
}

impl Drop for VpxDecoder {
    fn drop(&mut self) {
        // SAFETY: ctx was initialised in new() and is destroyed once.
        unsafe {
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}
