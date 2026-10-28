//! A live recording's pictures: Windows' H.264, HEVC or AV1 decoder on the
//! graphics card, then the video processor to BGRA.

use super::{Gpu, choose_output, drain, feed};
use crate::live::{LiveFrame, VideoCodec};
use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
    },
    core::{GUID, Interface},
};

/// What decoded pictures are: their size, and NV12, or P010 for 10 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Kind {
    pub size: (u32, u32),
    pub format: DXGI_FORMAT,
}

pub(super) struct Decoder {
    transform: IMFTransform,
    /// The pictures, as the stream last set them.
    kind: Kind,
    /// What to decode to when the decoder offers it.
    wanted: GUID,
    _manager: IMFDXGIDeviceManager,
}

// SAFETY: Media Foundation's decoder and device manager are free-threaded
// (created in the multithreaded apartment); the player uses the decoder
// from one thread at a time, behind a mutex.
unsafe impl Send for Decoder {}

impl Decoder {
    pub(super) fn new(gpu: &Gpu, w: u32, h: u32, fps: u32, codec: VideoCodec) -> Result<Self> {
        let (coded, wanted) = subtypes(codec);
        unsafe {
            let transform = decoder(codec)?;
            let attributes = transform.GetAttributes()?;
            // Each picture out as soon as it is decoded: the recorder makes
            // no B-frames, so there is nothing to wait for.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
            ensure!(
                attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0,
                "the decoder cannot use the graphics card"
            );
            let (mut token, mut manager) = (0, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &coded)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
            transform
                .SetInputType(0, &input, 0)
                .context("the decoder does not take this video")?;
            let format = choose(&transform, wanted)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                transform,
                kind: Kind {
                    size: (w, h),
                    format,
                },
                wanted,
                _manager: manager,
            })
        }
    }

    /// The pictures, as the stream last set them.
    pub(super) fn kind(&self) -> Kind {
        self.kind
    }

    /// Decode `frame`: each picture that comes out, its time, where it is
    /// (a texture, and its index in the texture array) and what it is, to
    /// `picture`. That changes where the stream does: the size where the
    /// sender changed its quality, and HEVC's format once the decoder has
    /// read that it is 10 bits.
    pub(super) fn decode(
        &mut self,
        frame: &LiveFrame,
        mut picture: impl FnMut(i64, &ID3D11Texture2D, u32, Kind) -> Result<()>,
    ) -> Result<()> {
        let (transform, wanted) = (&self.transform, self.wanted);
        // Read by each picture, set by a change of stream between them.
        let kind = std::cell::Cell::new(self.kind);
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
                    kind.get(),
                )
            }
        };
        // The decoder brings its own samples, on the graphics card. When the
        // stream changes it says so before the first picture of the new kind.
        let mut changed = || -> Result<()> {
            let format = choose(transform, wanted)?;
            let (w, h) = picture_size(transform)?;
            log::info!("the live pictures are now {w}x{h}, {format:?}");
            kind.set(Kind {
                size: (w, h),
                format,
            });
            Ok(())
        };
        let mut empty = || drain(transform, || Ok(None), &mut changed, &mut out);
        let decoded = feed(transform, frame, &mut empty).and_then(|()| empty());
        self.kind = kind.get();
        decoded
    }
}

/// The size of the decoder's pictures: the part shown, without the rows
/// that round the coded picture up to whole macroblocks.
fn picture_size(transform: &IMFTransform) -> Result<(u32, u32)> {
    unsafe {
        let output = transform.GetOutputCurrentType(0)?;
        let mut area = MFVideoArea::default();
        let shown = output.GetBlob(
            &MF_MT_MINIMUM_DISPLAY_APERTURE,
            std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), size_of::<MFVideoArea>()),
            None,
        );
        if shown.is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
            return Ok((area.Area.cx as u32, area.Area.cy as u32));
        }
        let packed = output.GetUINT64(&MF_MT_FRAME_SIZE)?;
        Ok(((packed >> 32) as u32, packed as u32))
    }
}

/// Have the decoder write `wanted` if it offers it, NV12 if not (HEVC's
/// offers P010 only once it has read that the stream is 10 bits): which.
fn choose(transform: &IMFTransform, wanted: GUID) -> Result<DXGI_FORMAT> {
    let offered = |subtype: GUID| unsafe {
        (0..)
            .map_while(|index| transform.GetOutputAvailableType(0, index).ok())
            .any(|offered| offered.GetGUID(&MF_MT_SUBTYPE).ok() == Some(subtype))
    };
    let subtype = if offered(wanted) {
        wanted
    } else {
        MFVideoFormat_NV12
    };
    choose_output(transform, |offered| unsafe {
        offered.GetGUID(&MF_MT_SUBTYPE).ok() == Some(subtype)
    })?;
    Ok(if subtype == MFVideoFormat_P010 {
        DXGI_FORMAT_P010
    } else {
        DXGI_FORMAT_NV12
    })
}

/// The coded pictures' subtype, and what they decode to.
fn subtypes(codec: VideoCodec) -> (GUID, GUID) {
    match codec {
        VideoCodec::H264 => (MFVideoFormat_H264, MFVideoFormat_NV12),
        VideoCodec::Hevc => (MFVideoFormat_HEVC, MFVideoFormat_P010),
        VideoCodec::Av1 => (MFVideoFormat_AV1, MFVideoFormat_P010),
    }
}

/// Whether this PC can decode `codec`: H.264 always; HEVC with the HEVC
/// Video Extensions installed, AV1 with the AV1 Video Extension.
pub(super) fn decodes(codec: VideoCodec) -> bool {
    codec == VideoCodec::H264 || installed(codec).is_ok_and(|found| found.is_some())
}

/// Windows' decoder for `codec`.
fn decoder(codec: VideoCodec) -> Result<IMFTransform> {
    unsafe {
        match codec {
            VideoCodec::H264 => {
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .context("no H.264 decoder")
            }
            VideoCodec::Hevc | VideoCodec::Av1 => installed(codec)?
                .with_context(|| format!("this PC has no {codec:?} decoder"))?
                .ActivateObject()
                .with_context(|| format!("the {codec:?} decoder did not start")),
        }
    }
}

/// The best decoder installed for `codec`, as Windows lists them: one that
/// comes from the Store, as HEVC's does, too.
fn installed(codec: VideoCodec) -> Result<Option<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtypes(codec).0,
    };
    let (mut found, mut count) = (std::ptr::null_mut(), 0);
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            None,
            &mut found,
            &mut count,
        )?;
        if found.is_null() {
            return Ok(None);
        }
        // SAFETY: MFTEnumEx hands over an array of `count` activation
        // objects, each to release, in memory to free with CoTaskMemFree.
        let list = std::slice::from_raw_parts_mut(found, count as usize);
        let best = list.first_mut().and_then(Option::take);
        for rest in list.iter_mut() {
            drop(rest.take());
        }
        CoTaskMemFree(Some(found as *const _));
        Ok(best)
    }
}

/// A BGRA texture the video processor can write a picture into.
fn bgra(
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

/// Decoded pictures converted to BGRA.
pub(super) struct Pictures {
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    /// The decoder's picture, copied out of its texture array: NV12, or
    /// P010 for 10 bits.
    decoded: ID3D11Texture2D,
    input: ID3D11VideoProcessorInputView,
    /// The picture converted last.
    bgra: ID3D11Texture2D,
    output: ID3D11VideoProcessorOutputView,
    kind: Kind,
}

impl Pictures {
    pub(super) fn new(gpu: &Gpu, kind: Kind, fps: u32) -> Result<Self> {
        let Kind {
            size: (w, h),
            format,
        } = kind;
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
            let support = enumerator.CheckVideoProcessorFormat(format)?;
            ensure!(
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                "the video processor cannot read {format:?}"
            );
            let decoded = gpu
                .texture(
                    w,
                    h,
                    format,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )
                .context("no texture for the decoded pictures")?;
            let mut input = None;
            device
                .CreateVideoProcessorInputView(
                    &decoded,
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
            let (bgra, output) = bgra(gpu, &device, &enumerator, w, h)?;
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
                decoded,
                input: input.context("no input view")?,
                bgra,
                output,
                kind,
            })
        }
    }

    /// Convert a decoded picture, at `index` of `texture`.
    pub(super) fn convert(
        &mut self,
        gpu: &Gpu,
        texture: &ID3D11Texture2D,
        index: u32,
    ) -> Result<()> {
        let (w, h) = self.kind.size;
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
            gpu.context.CopySubresourceRegion(
                &self.decoded,
                0,
                0,
                0,
                0,
                texture,
                index,
                Some(&area),
            );
        }
        let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        }];
        let result = unsafe {
            self.video
                .VideoProcessorBlt(&self.processor, &self.output, 0, &streams)
        };
        // Release the reference the stream took.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt failed")
    }

    pub(super) fn kind(&self) -> Kind {
        self.kind
    }

    /// Copy the picture converted last into `target`, if it is the
    /// picture's size: whether it did.
    pub(super) fn copy_to(&self, gpu: &Gpu, target: &ID3D11Texture2D) -> bool {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { target.GetDesc(&mut desc) };
        if (desc.Width, desc.Height) != self.kind.size {
            return false;
        }
        unsafe { gpu.context.CopyResource(target, &self.bgra) };
        true
    }
}
