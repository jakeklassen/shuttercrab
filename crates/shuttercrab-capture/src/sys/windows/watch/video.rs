//! A live recording's pictures: Windows' H.264 decoder on the graphics
//! card, then the video processor to BGRA, a few pictures kept until they
//! are due.

use super::{Gpu, choose_output, drain, feed};
use crate::live::LiveFrame;
use anyhow::{Context, Result, ensure};
use std::collections::VecDeque;
use windows::{
    Win32::{
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::Interface,
};

pub(super) struct Decoder {
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
}

// SAFETY: Media Foundation's decoder and device manager are free-threaded
// (created in the multithreaded apartment); the player uses the decoder
// from one thread at a time, behind a mutex.
unsafe impl Send for Decoder {}

impl Decoder {
    pub(super) fn new(gpu: &Gpu, w: u32, h: u32, fps: u32) -> Result<Self> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .context("no H.264 decoder")?;
            let attributes = transform.GetAttributes()?;
            // Each picture out as soon as it is decoded: the recorder makes
            // no B-frames, so there is nothing to wait for.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
            ensure!(
                attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0,
                "the H.264 decoder cannot use the graphics card"
            );
            let (mut token, mut manager) = (0, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
            transform
                .SetInputType(0, &input, 0)
                .context("the H.264 decoder does not take this video")?;
            choose_nv12(&transform)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                transform,
                _manager: manager,
            })
        }
    }

    /// Decode `frame`: each picture that comes out, its time and where it
    /// is (a texture, and its index in the texture array), to `picture`.
    pub(super) fn decode(
        &mut self,
        frame: &LiveFrame,
        mut picture: impl FnMut(i64, &ID3D11Texture2D, u32) -> Result<()>,
    ) -> Result<()> {
        let transform = &self.transform;
        let mut out = |sample: IMFSample| -> Result<()> {
            unsafe {
                let buffer: IMFDXGIBuffer = sample.GetBufferByIndex(0)?.cast()?;
                let mut raw = std::ptr::null_mut();
                buffer.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
                // SAFETY: GetResource hands over a reference to the texture.
                let texture = ID3D11Texture2D::from_raw(raw);
                picture(
                    sample.GetSampleTime()?,
                    &texture,
                    buffer.GetSubresourceIndex()?,
                )
            }
        };
        // The decoder brings its own samples, on the graphics card.
        let mut empty = || drain(transform, || Ok(None), || choose_nv12(transform), &mut out);
        feed(transform, frame, &mut empty)?;
        empty()
    }
}

fn choose_nv12(transform: &IMFTransform) -> Result<()> {
    choose_output(transform, |offered| unsafe {
        offered.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12)
    })
}

/// How many decoded pictures are kept until they are due.
const SLOTS: usize = 4;

/// A BGRA texture the video processor can write a picture into.
fn bgra_slot(
    gpu: &Gpu,
    device: &ID3D11VideoDevice,
    enumerator: &ID3D11VideoProcessorEnumerator,
    w: u32,
    h: u32,
) -> Result<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)> {
    let texture = gpu
        .texture(
            w,
            h,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
            None,
        )
        .context("no BGRA texture")?;
    let mut view = None;
    unsafe {
        device.CreateVideoProcessorOutputView(
            &texture,
            enumerator,
            &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            },
            Some(&mut view),
        )
    }
    .context("no output view")?;
    Ok((texture, view.context("no output view")?))
}

/// Decoded pictures, in BGRA, until they are due.
pub(super) struct Pictures {
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    /// The decoder's picture, copied out of its texture array.
    nv12: ID3D11Texture2D,
    input: ID3D11VideoProcessorInputView,
    slots: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    /// Pictures not drawn yet, oldest first: their times and slots.
    waiting: VecDeque<(i64, usize)>,
    size: (u32, u32),
}

impl Pictures {
    pub(super) fn new(gpu: &Gpu, w: u32, h: u32, fps: u32) -> Result<Self> {
        let device: ID3D11VideoDevice = gpu.device.cast().context("no video device")?;
        let video: ID3D11VideoContext1 = gpu.context.cast().context("no video context")?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: w,
            InputHeight: h,
            OutputFrameRate: rate,
            OutputWidth: w,
            OutputHeight: h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = device
                .CreateVideoProcessorEnumerator(&content)
                .context("no video processor for this size")?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?;
            ensure!(
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0,
                "the video processor cannot write BGRA"
            );
            let processor = device.CreateVideoProcessor(&enumerator, 0)?;
            let nv12 = gpu
                .texture(
                    w,
                    h,
                    DXGI_FORMAT_NV12,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )
                .context("no NV12 texture")?;
            let mut input = None;
            device
                .CreateVideoProcessorInputView(
                    &nv12,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPIV {
                                MipSlice: 0,
                                ArraySlice: 0,
                            },
                        },
                    },
                    Some(&mut input),
                )
                .context("no input view")?;
            let slots = (0..SLOTS)
                .map(|_| bgra_slot(gpu, &device, &enumerator, w, h))
                .collect::<Result<_>>()?;
            // What the recorder writes: Rec.709, studio range.
            video.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            video.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            Ok(Self {
                video,
                processor,
                nv12,
                input: input.context("no input view")?,
                slots,
                waiting: VecDeque::with_capacity(SLOTS),
                size: (w, h),
            })
        }
    }

    /// Convert a decoded picture, at `index` of `texture`, to keep until
    /// it is due. With every slot waiting, the oldest is dropped.
    pub(super) fn add(
        &mut self,
        gpu: &Gpu,
        time: i64,
        texture: &ID3D11Texture2D,
        index: u32,
    ) -> Result<()> {
        if self.waiting.len() == SLOTS {
            self.waiting.pop_front();
        }
        let slot = (0..SLOTS)
            .find(|s| self.waiting.iter().all(|(_, w)| w != s))
            .unwrap_or_default();
        let (w, h) = self.size;
        // The decoder's pictures are taller than the video (rounded up to
        // whole macroblocks).
        let area = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: w,
            bottom: h,
            back: 1,
        };
        unsafe {
            gpu.context
                .CopySubresourceRegion(&self.nv12, 0, 0, 0, 0, texture, index, Some(&area));
        }
        let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        }];
        let result = unsafe {
            self.video
                .VideoProcessorBlt(&self.processor, &self.slots[slot].1, 0, &streams)
        };
        // Release the reference the stream took.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt failed")?;
        self.waiting.push_back((time, slot));
        Ok(())
    }

    /// Copy the newest picture due by `due` (all are, if `None`) into
    /// `target`, dropping the ones before it: its time, if there was one.
    pub(super) fn draw(
        &mut self,
        gpu: &Gpu,
        target: &ID3D11Texture2D,
        due: Option<i64>,
    ) -> Result<Option<i64>> {
        let newest = match due {
            Some(due) => self.waiting.iter().rposition(|&(time, _)| time <= due),
            None => self.waiting.len().checked_sub(1),
        };
        let Some(newest) = newest else {
            return Ok(None);
        };
        let (time, slot) = self.waiting[newest];
        self.waiting.drain(..=newest);
        unsafe { gpu.context.CopyResource(target, &self.slots[slot].0) };
        Ok(Some(time))
    }
}
