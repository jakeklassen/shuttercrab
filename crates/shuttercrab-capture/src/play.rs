//! Playing a recording back. Media Foundation's Media Engine opens the file,
//! decodes it (on the graphics card where it can) and plays its sound itself;
//! the app asks it for each new picture, scaled to the size it shows it at.

use crate::gpu::Gpu;
use anyhow::{Context, Result};
use std::{os::windows::ffi::OsStrExt, path::Path, time::Duration};
use windows::{
    Win32::{
        Foundation::{RECT, S_OK},
        Graphics::{
            Direct3D11::*,
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                IDXGIDevice, IDXGIOutput,
            },
        },
        Media::MediaFoundation::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::{BSTR, Interface, implement},
};

/// What the Media Engine tells the app about the file it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The file is open: its length and picture size are known.
    Loaded,
    /// The first picture is ready to show.
    FirstFrame,
    Playing,
    Paused,
    /// A seek finished: the picture at the new time is ready.
    Seeked,
    /// Playing reached the end.
    Ended,
    /// The file cannot be played.
    Failed(String),
}

type Listener = Box<dyn Fn(PlayerEvent) + Send + Sync>;

/// One recording, open for playing.
pub struct Player {
    engine: IMFMediaEngine,
    gpu: Gpu,
    _manager: IMFDXGIDeviceManager,
    /// The textures the current picture size uses.
    picture: Option<Picture>,
    /// The display whose refresh paces the pictures, once asked for.
    output: Option<IDXGIOutput>,
}

/// A picture size's textures: the one the Media Engine draws into, and the
/// one the CPU reads it back from.
struct Picture {
    width: u32,
    height: u32,
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

impl Player {
    /// Open `path`, paused at the start. `listener` hears about loading,
    /// playing and errors, on one of Media Foundation's threads. The calling
    /// thread must be in COM's multithreaded apartment.
    pub fn open(
        path: &Path,
        listener: impl Fn(PlayerEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        let gpu = Gpu::video(None)?;
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("MFStartup failed")?;
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3)?;
            let attributes = attributes.context("no attributes")?;
            let notify: IMFMediaEngineNotify = Notify {
                listener: Box::new(listener),
            }
            .into();
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            // A device manager puts the engine in frame server mode: it draws
            // each picture into a texture the app gives it.
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)?;
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .context("no Media Engine")?;
            let engine = factory
                .CreateInstance(0, &attributes)
                .context("cannot make a Media Engine")?;
            let player = Self {
                engine,
                gpu,
                _manager: manager,
                picture: None,
                output: None,
            };
            let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            player
                .engine
                .SetSource(&BSTR::from_wide(&wide))
                .with_context(|| format!("cannot open {}", path.display()))?;
            Ok(player)
        }
    }

    /// The pictures' size, once the file has loaded.
    pub fn size(&self) -> Option<(u32, u32)> {
        let (mut width, mut height) = (0, 0);
        unsafe {
            self.engine
                .GetNativeVideoSize(Some(&mut width), Some(&mut height))
                .ok()?;
        }
        (width > 0 && height > 0).then_some((width, height))
    }

    /// How long the recording is, once the file has loaded.
    pub fn duration(&self) -> Duration {
        seconds(unsafe { self.engine.GetDuration() })
    }

    /// Where playing is.
    pub fn position(&self) -> Duration {
        seconds(unsafe { self.engine.GetCurrentTime() })
    }

    pub fn play(&self) -> Result<()> {
        unsafe { self.engine.Play() }.context("cannot play")
    }

    pub fn pause(&self) -> Result<()> {
        unsafe { self.engine.Pause() }.context("cannot pause")
    }

    pub fn is_paused(&self) -> bool {
        unsafe { self.engine.IsPaused() }.as_bool()
    }

    pub fn is_ended(&self) -> bool {
        unsafe { self.engine.IsEnded() }.as_bool()
    }

    /// Go to `at`; [`PlayerEvent::Seeked`] follows.
    pub fn seek(&self, at: Duration) -> Result<()> {
        unsafe { self.engine.SetCurrentTime(at.as_secs_f64()) }.context("cannot seek")
    }

    /// The sound's volume, 0 to 1.
    pub fn set_volume(&self, volume: f64) -> Result<()> {
        unsafe { self.engine.SetVolume(volume.clamp(0.0, 1.0)) }.context("cannot set the volume")
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        unsafe { self.engine.SetMuted(muted) }.context("cannot mute")
    }

    /// Wait for the next refresh of the display the player's graphics card
    /// drives first: the pace to ask for pictures at.
    pub fn wait_for_refresh(&mut self) -> Result<()> {
        let output = match &self.output {
            Some(output) => output,
            None => {
                let output = unsafe {
                    self.gpu
                        .device
                        .cast::<IDXGIDevice>()?
                        .GetAdapter()?
                        .EnumOutputs(0)
                }
                .context("the graphics card drives no display")?;
                self.output.insert(output)
            }
        };
        unsafe { output.WaitForVBlank() }.context("WaitForVBlank failed")
    }

    /// The picture to show now, `width` × `height` BGRA8 (letterboxed in
    /// black if the shape differs), when it has changed since the last call.
    pub fn next_frame(&mut self, width: u32, height: u32) -> Result<Option<Vec<u8>>> {
        let mut time = 0i64;
        // S_FALSE means there is no new picture; windows-rs would call that
        // success too, so look at the code itself.
        let code = unsafe {
            (Interface::vtable(&self.engine).OnVideoStreamTick)(self.engine.as_raw(), &mut time)
        };
        if code != S_OK {
            code.ok()?;
            return Ok(None);
        }
        self.frame(width, height).map(Some)
    }

    /// The current picture, `width` × `height` BGRA8, new or not: for a new
    /// size.
    pub fn frame(&mut self, width: u32, height: u32) -> Result<Vec<u8>> {
        let (width, height) = (width.max(1), height.max(1));
        if self
            .picture
            .as_ref()
            .is_none_or(|p| (p.width, p.height) != (width, height))
        {
            self.picture = None;
            self.picture = Some(Picture::new(&self.gpu, width, height)?);
        }
        let picture = self.picture.as_ref().context("no picture")?;
        let area = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let black = MFARGB {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbAlpha: 255,
        };
        unsafe {
            self.engine
                .TransferVideoFrame(&picture.target, None, &area, Some(&black))
                .context("TransferVideoFrame failed")?;
        }
        picture.read(&self.gpu)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe {
            let _ = self.engine.Shutdown();
        }
        self.picture = None;
        self.gpu.trim();
        unsafe {
            let _ = MFShutdown();
        }
    }
}

impl Picture {
    fn new(gpu: &Gpu, width: u32, height: u32) -> Result<Self> {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let (mut target, mut staging) = (None, None);
        unsafe {
            gpu.device.CreateTexture2D(&desc, None, Some(&mut target))?;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
        }
        Ok(Self {
            width,
            height,
            target: target.context("no picture texture")?,
            staging: staging.context("no staging texture")?,
        })
    }

    /// The picture's pixels, tightly packed.
    fn read(&self, gpu: &Gpu) -> Result<Vec<u8>> {
        let row = self.width as usize * 4;
        let mut bytes = vec![0u8; row * self.height as usize];
        unsafe {
            gpu.context.CopyResource(&self.staging, &self.target);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            for (y, out) in bytes.chunks_exact_mut(row).enumerate() {
                // SAFETY: the staging texture is `width` BGRA8 pixels wide
                // and `height` rows tall, so row `y` starts at
                // `y * RowPitch` in the mapping and holds `row` bytes. Bytes
                // have no alignment or validity rules, and the row is copied
                // out before `Unmap`.
                let src = std::slice::from_raw_parts(
                    map.pData.cast::<u8>().add(y * map.RowPitch as usize),
                    row,
                );
                out.copy_from_slice(src);
            }
            gpu.context.Unmap(&self.staging, 0);
        }
        Ok(bytes)
    }
}

/// The Media Engine's seconds as a duration: zero for its "not known" (NaN)
/// and anything below zero, the longest duration for a live stream's
/// infinity.
fn seconds(value: f64) -> Duration {
    if value.is_nan() || value <= 0.0 {
        Duration::ZERO
    } else {
        Duration::try_from_secs_f64(value).unwrap_or(Duration::MAX)
    }
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    listener: Listener,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        let event = match MF_MEDIA_ENGINE_EVENT(event as i32) {
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => PlayerEvent::Loaded,
            MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY => PlayerEvent::FirstFrame,
            MF_MEDIA_ENGINE_EVENT_PLAYING => PlayerEvent::Playing,
            MF_MEDIA_ENGINE_EVENT_PAUSE => PlayerEvent::Paused,
            MF_MEDIA_ENGINE_EVENT_SEEKED => PlayerEvent::Seeked,
            MF_MEDIA_ENGINE_EVENT_ENDED => PlayerEvent::Ended,
            // `param1` is the MF_MEDIA_ENGINE_ERR, `param2` the HRESULT.
            MF_MEDIA_ENGINE_EVENT_ERROR => PlayerEvent::Failed(format!(
                "{} (Media Engine error {param1})",
                windows::core::HRESULT(param2 as i32).message()
            )),
            _ => return Ok(()),
        };
        (self.listener)(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_negative_times_are_zero() {
        assert_eq!(seconds(f64::NAN), Duration::ZERO);
        assert_eq!(seconds(-1.0), Duration::ZERO);
        assert_eq!(seconds(1.5), Duration::from_millis(1500));
        assert_eq!(seconds(f64::INFINITY), Duration::MAX);
    }
}
