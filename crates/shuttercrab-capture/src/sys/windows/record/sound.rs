//! A recording's sound: what the speakers play (Windows' loopback, WASAPI,
//! on the default output device), and a microphone, each captured on a
//! thread of its own while switched on.
//!
//! Windows converts each to the track's format (48 kHz, 16-bit stereo),
//! whatever the device runs at, and stamps each packet with the time of its
//! first sample on the same clock as the video frames (QPC). The recording
//! thread mixes them with a [`Mixer`]. A microphone switched off is not
//! opened at all, so Windows does not show it as in use.

use super::{Event, qpc_ticks};
pub(super) use crate::record::mix::{CHANNELS, Mixer, RATE, frames_to_ticks};
use crate::record::{Microphone, Source, TICKS_PER_SECOND};
use anyhow::{Context, Result};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::JoinHandle,
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
            AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, DEVICE_STATE_ACTIVE,
            IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
            IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVE_FORMAT_PCM, WAVEFORMATEX, eCapture,
            eConsole, eRender,
        },
        System::{
            Com::{
                BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize, IAgileObject, IAgileObject_Impl, STGM_READ,
                StructuredStorage::PROPVARIANT,
            },
            Threading::{CreateEventW, SetEvent, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
    core::{HRESULT, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement},
};

/// Which device a capture records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Device {
    /// What the speakers play, or Windows' default microphone.
    Default,
    /// A microphone, by Windows' id.
    Microphone(String),
    /// What one process and the processes it started play, in place of
    /// all the speakers do (a recorded window's app).
    Process(u32),
}

/// The microphones plugged in and on, Windows' default first.
pub fn microphones() -> Result<Vec<Microphone>> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("no audio device enumerator")?;
        let default = devices
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let all = devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut found = Vec::new();
        for i in 0..all.GetCount()? {
            let device = all.Item(i)?;
            found.push(Microphone {
                id: device_id(&device)?,
                name: device_name(&device).unwrap_or_else(|_| "Microphone".into()),
            });
        }
        found.sort_by_key(|m| Some(&m.id) != default.as_ref());
        Ok(found)
    }
}

fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast()));
        Ok(text?)
    }
}

fn device_name(device: &IMMDevice) -> Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let name = store.GetValue(&PKEY_Device_FriendlyName)?;
        Ok(name.to_string())
    }
}

/// Sound from one source, captured from `captured` (QPC ticks, its first
/// frame) on: 16-bit samples, the channels interleaved.
pub(super) struct Packet {
    pub(super) source: Source,
    pub(super) captured: i64,
    pub(super) samples: Vec<i16>,
}

/// How much sound a device buffers, 100 ns ticks: room for the thread to be
/// late now and then.
const BUFFER: i64 = TICKS_PER_SECOND / 5;

/// How long a thread waits for sound before checking whether to stop.
const WAKE_MS: u32 = 100;

/// One source's capture, running on its own thread until dropped.
pub(super) struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start capturing `source` from `device`, sending each packet to the
    /// recording thread as [`Event::Sound`]. Returns once the capture is
    /// running, or why it could not start.
    pub(super) fn start(source: Source, device: Device, events: Sender<Event>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, started) = std::sync::mpsc::channel::<Result<()>>();
        let stopping = stop.clone();
        let name = match source {
            Source::System => "shuttercrab-system-sound",
            Source::Microphone => "shuttercrab-microphone",
        };
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || capture(source, device, &events, &stopping, ready))
            .context("could not start the sound thread")?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                anyhow::bail!("the sound thread stopped")
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A capture thread: open the device, say whether it started, then pass on
/// packets until told to stop. A device that goes away (headphones
/// unplugged) is replaced by the new default one; a process is listened to
/// again.
fn capture(
    source: Source,
    mut device: Device,
    events: &Sender<Event>,
    stop: &AtomicBool,
    ready: std::sync::mpsc::Sender<Result<()>>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut ready = Some(ready);
    while !stop.load(Ordering::Acquire) {
        let stream = match Stream::open(source, &device) {
            Ok(stream) => stream,
            Err(e) => {
                match ready.take() {
                    Some(ready) => {
                        let _ = ready.send(Err(e));
                        break;
                    }
                    // Mid-recording: wait for a device, and try again.
                    None => log::warn!("no {source:?} device to record: {e:#}"),
                }
                // The chosen microphone first; once it has gone, the
                // default one.
                if matches!(device, Device::Microphone(_)) {
                    device = Device::Default;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        };
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        match stream.pass_on(source, events, stop) {
            Ok(()) => break,
            Err(e) => {
                log::warn!("the {source:?} device changed or went away: {e:#}; reopening");
                if matches!(device, Device::Microphone(_)) {
                    device = Device::Default;
                }
            }
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// An open capture stream: the output device's loopback, or a microphone.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl Stream {
    fn open(source: Source, device: &Device) -> Result<Self> {
        unsafe {
            let client = match (source, device) {
                (Source::System, Device::Process(id)) => process_client(*id)?,
                _ => device_client(source, device)?,
            };
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: CHANNELS,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * u32::from(CHANNELS) * 2,
                nBlockAlign: CHANNELS * 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            // Windows converts from the device's own format to the track's.
            let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            if source == Source::System {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, &format, None)
                .context("could not start capturing sound")?;
            let event = CreateEventW(None, false, false, None).context("no event")?;
            let opened = (|| {
                client.SetEventHandle(event)?;
                let capture: IAudioCaptureClient = client.GetService()?;
                client.Start()?;
                anyhow::Ok(capture)
            })();
            match opened {
                Ok(capture) => Ok(Self {
                    client,
                    capture,
                    event,
                }),
                Err(e) => {
                    let _ = CloseHandle(event);
                    Err(e.context("could not start capturing sound"))
                }
            }
        }
    }

    /// Send every packet on until `stop`; an error means the device went
    /// away.
    fn pass_on(&self, source: Source, events: &Sender<Event>, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            let woken = unsafe { WaitForSingleObject(self.event, WAKE_MS) };
            if woken != WAIT_OBJECT_0 {
                continue;
            }
            while unsafe { self.capture.GetNextPacketSize() }? > 0 {
                let packet = self.next_packet(source)?;
                if events.send(Event::Sound(packet)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn next_packet(&self, source: Source) -> Result<Packet> {
        unsafe {
            let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
            let count = frames as usize * usize::from(CHANNELS);
            let samples = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                vec![0; count]
            } else {
                std::slice::from_raw_parts(data.cast::<i16>(), count).to_vec()
            };
            self.capture.ReleaseBuffer(frames)?;
            // Sound cannot have been captured after it arrived. The speakers'
            // loopback stamps it a fifth of a second ahead (when it will be
            // heard, it seems), and a sync test recorded it that much behind
            // the picture; a process's sound may come with no time at all.
            let arrived = qpc_ticks() - frames_to_ticks(u64::from(frames));
            let timed = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && qpc != 0;
            let captured = if timed {
                (qpc as i64).min(arrived)
            } else {
                arrived
            };
            Ok(Packet {
                source,
                captured,
                samples,
            })
        }
    }
}

/// A client for a device: the speakers' loopback, or a microphone.
unsafe fn device_client(source: Source, device: &Device) -> Result<IAudioClient> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("no audio device enumerator")?;
        let device = match (source, device) {
            (Source::System, _) => devices
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .context("no default output device")?,
            (Source::Microphone, Device::Microphone(id)) => devices
                .GetDevice(PCWSTR(HSTRING::from(id.as_str()).as_ptr()))
                .context("the chosen microphone is not there")?,
            (Source::Microphone, _) => devices
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .context("no microphone")?,
        };
        device
            .Activate(CLSCTX_ALL, None)
            .context("could not open the sound device")
    }
}

/// A client for what process `id` and the processes it started play
/// (Windows 10 2004 and later): Windows hands it over asynchronously.
unsafe fn process_client(id: u32) -> Result<IAudioClient> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: id,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // Never dropped: dropping clears it, which frees the blob, and the
        // blob is `params` here on the stack.
        let mut variant = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&raw const params).cast_mut().cast(),
        };
        let done = CreateEventW(None, false, false, None).context("no event")?;
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(done).into();
        let activated = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *variant),
            &handler,
        )
        .context("could not ask for the app's sound")
        .and_then(|operation| {
            WaitForSingleObject(done, 5_000);
            let (mut result, mut client) = (HRESULT(0), None::<IUnknown>);
            operation.GetActivateResult(&mut result, &mut client)?;
            result
                .ok()
                .context("Windows would not give the app's sound")?;
            Ok(client
                .context("no sound client for the app")?
                .cast::<IAudioClient>()?)
        });
        let _ = CloseHandle(done);
        activated
    }
}

/// Says when Windows has activated a process's sound client. Agile, as
/// Windows calls it from a thread of its own.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated(HANDLE);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        unsafe { SetEvent(self.0) }
    }
}

impl IAgileObject_Impl for Activated_Impl {}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}
