//! From converted frames to an MP4: RGBA to NV12 in the Direct3D video
//! processor, then H.264 through Media Foundation's sink writer.

use crate::gpu::Gpu;
use anyhow::{Context, Result, ensure};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
    },
    core::{HSTRING, Interface, Ref, implement},
};

/// RGBA → NV12 (BT.709, studio range) with the Direct3D video processor,
/// into a small pool of textures the encoder reads.
pub(super) struct Nv12 {
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    slots: Vec<Slot>,
}

struct Slot {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    /// Samples of it the encoder still holds: a repeated frame (a still
    /// screen, the stop) can be with it more than once. Counted off as Media
    /// Foundation releases them.
    busy: Arc<AtomicUsize>,
    release: IMFAsyncCallback,
}

/// How many frames can be with the encoder at once.
const SLOTS: usize = 4;

impl Nv12 {
    pub(super) fn new(gpu: &Gpu, rgba: &ID3D11Texture2D, w: u32, h: u32, fps: u32) -> Result<Self> {
        let device: ID3D11VideoDevice = gpu.device.cast().context("no video device")?;
        let video: ID3D11VideoContext1 = gpu.context.cast().context("no video context")?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            InputWidth: w,
            InputHeight: h,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            OutputWidth: w,
            OutputHeight: h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = device.CreateVideoProcessorEnumerator(&content)?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM)?;
            ensure!(
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                "the video processor cannot read RGBA"
            );
            let processor = device.CreateVideoProcessor(&enumerator, 0)?;
            let mut input = None;
            device.CreateVideoProcessorInputView(
                rgba,
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
            )?;
            let mut slots = Vec::with_capacity(SLOTS);
            for _ in 0..SLOTS {
                let texture = gpu.texture(
                    w,
                    h,
                    DXGI_FORMAT_NV12,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )?;
                let mut view = None;
                device.CreateVideoProcessorOutputView(
                    &texture,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                let busy = Arc::new(AtomicUsize::new(0));
                slots.push(Slot {
                    texture,
                    view: view.context("no output view")?,
                    release: Released(busy.clone()).into(),
                    busy,
                });
            }
            video.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
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
                input: input.context("no input view")?,
                slots,
            })
        }
    }

    pub(super) fn free_slot(&self) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.busy.load(Ordering::Acquire) == 0)
    }

    pub(super) fn convert(&self, _gpu: &Gpu, slot: usize) -> Result<()> {
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        };
        let streams = [stream];
        let result = unsafe {
            self.video
                .VideoProcessorBlt(&self.processor, &self.slots[slot].view, 0, &streams)
        };
        // Release the reference the stream took.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt failed")
    }
}

/// Counts a use of a slot off when Media Foundation releases its sample.
#[implement(IMFAsyncCallback)]
struct Released(Arc<AtomicUsize>);

impl IMFAsyncCallback_Impl for Released_Impl {
    fn GetParameters(&self, _: *mut u32, _: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        self.0.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

/// An H.264 MP4 file written by Media Foundation's sink writer.
pub(super) struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    _manager: IMFDXGIDeviceManager,
    finished: bool,
}

impl Mp4Writer {
    /// With `hardware`, Media Foundation may choose a hardware encoder.
    pub(super) fn new(
        gpu: &Gpu,
        path: &Path,
        w: u32,
        h: u32,
        fps: u32,
        hardware: bool,
    ) -> Result<Self> {
        unsafe {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.context("no attributes")?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
            // MP4 whatever the file is called: the app writes to a
            // `.partial` name and renames the file once it is finished.
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;

            let _ = std::fs::remove_file(path);
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, &attributes)
                .with_context(|| format!("cannot write {}", path.display()))?;

            // PRD §13.1: H.264, Rec.709. About 0.1 bit per pixel per frame,
            // within 2–80 Mbit/s.
            let bitrate = ((w as u64 * h as u64 * fps as u64) / 10).clamp(2_000_000, 80_000_000);
            let output = video_type(&MFVideoFormat_H264, w, h, fps)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate as u32)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            let stream = writer.AddStream(&output)?;
            let input = video_type(&MFVideoFormat_NV12, w, h, fps)?;
            writer
                .SetInputMediaType(stream, &input, None)
                .context("the encoder does not take NV12 at this size")?;
            writer.BeginWriting().context("BeginWriting failed")?;
            Ok(Self {
                writer,
                stream,
                _manager: manager,
                finished: false,
            })
        }
    }

    /// Hand the slot's texture to the encoder at `time`.
    pub(super) fn write(&self, nv12: &Nv12, slot: usize, time: i64, duration: i64) -> Result<()> {
        let slot = &nv12.slots[slot];
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &slot.texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateTrackedSample()?;
            sample.SetAllocator(&slot.release, None)?;
            let sample: IMFSample = sample.cast()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            slot.busy.fetch_add(1, Ordering::AcqRel);
            if let Err(e) = self.writer.WriteSample(self.stream, &sample) {
                slot.busy.fetch_sub(1, Ordering::AcqRel);
                return Err(e).context("WriteSample failed");
            }
        }
        Ok(())
    }

    /// Finish the file. Returns whether a hardware encoder was used.
    pub(super) fn finish(&mut self) -> Result<bool> {
        self.finished = true;
        let hardware = self.hardware_encoder();
        unsafe { self.writer.Finalize() }.context("could not finish the video file")?;
        Ok(hardware)
    }

    /// A hardware encoder carries `MFT_ENUM_HARDWARE_URL_Attribute`.
    fn hardware_encoder(&self) -> bool {
        unsafe {
            let mut raw = std::ptr::null_mut();
            if self
                .writer
                .GetServiceForStream(
                    self.stream,
                    &windows::core::GUID::zeroed(),
                    &IMFTransform::IID,
                    &mut raw,
                )
                .is_err()
                || raw.is_null()
            {
                return false;
            }
            let transform = IMFTransform::from_raw(raw);
            transform
                .GetAttributes()
                .and_then(|attributes| attributes.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute))
                .is_ok()
        }
    }
}

fn video_type(subtype: &windows::core::GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        // Rec.709, studio range: what the video processor writes and what
        // players expect of SDR H.264.
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        if !self.finished {
            let _ = unsafe { self.writer.Finalize() };
        }
    }
}
