//! Direct3D 11 device, texture helpers, and the GPU color transform.

use crate::color::{Highlights, TILE};
use anyhow::{Context, Result, anyhow, bail, ensure};
use windows::{
    Graphics::DirectX::Direct3D11::IDirect3DDevice,
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{Fxc::*, *},
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice,
    },
    core::{Interface, PCSTR, s},
};

const SHADER_SOURCE: &str = include_str!("../shaders/hdr_to_sdr.hlsl");

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
}

impl Gpu {
    /// A hardware device on `adapter`, or on the default adapter.
    pub fn hardware(adapter: Option<&IDXGIAdapter1>) -> Result<Self> {
        let driver = if adapter.is_some() {
            D3D_DRIVER_TYPE_UNKNOWN
        } else {
            D3D_DRIVER_TYPE_HARDWARE
        };
        let adapter = adapter.map(|a| a.cast::<IDXGIAdapter>()).transpose()?;
        Self::create(adapter.as_ref(), driver)
    }

    /// The WARP software rasterizer, for deterministic tests on any machine.
    pub fn warp() -> Result<Self> {
        Self::create(None, D3D_DRIVER_TYPE_WARP)
    }

    fn create(adapter: Option<&IDXGIAdapter>, driver: D3D_DRIVER_TYPE) -> Result<Self> {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                adapter,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice failed")?;
        }
        let device: ID3D11Device = device.context("D3D11 returned no device")?;
        let context: ID3D11DeviceContext = context.context("D3D11 returned no context")?;
        // WGC's free-threaded frame pool uses this device from its own thread.
        unsafe {
            let _ = context
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
        }
        let adapter_name = unsafe {
            let dxgi = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
            wide_to_string(&dxgi.Description)
        };
        Ok(Self {
            device,
            context,
            adapter_name,
        })
    }

    /// The same device, as the WinRT object Windows.Graphics.Capture takes.
    pub fn winrt_device(&self) -> Result<IDirect3DDevice> {
        let dxgi = self.device.cast::<IDXGIDevice>()?;
        Ok(unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?)
    }

    pub fn texture(
        &self,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
        initial: Option<(&[u8], u32)>,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0 as u32,
            ..Default::default()
        };
        let data = initial.map(|(bytes, pitch)| D3D11_SUBRESOURCE_DATA {
            pSysMem: bytes.as_ptr().cast(),
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        });
        let mut texture = None;
        unsafe {
            self.device.CreateTexture2D(
                &desc,
                data.as_ref().map(|d| d as *const _),
                Some(&mut texture),
            )?;
        }
        texture.context("CreateTexture2D returned no texture")
    }

    /// Upload tightly packed little-endian RGBA FP16 pixels.
    pub fn upload_rgba16f(&self, width: u32, height: u32, bytes: &[u8]) -> Result<ID3D11Texture2D> {
        ensure!(
            bytes.len() == (width * height * 8) as usize,
            "FP16 upload has the wrong size"
        );
        self.texture(
            width,
            height,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            D3D11_BIND_SHADER_RESOURCE,
            Some((bytes, width * 8)),
        )
    }

    /// Copy a texture to system memory, tightly packed.
    pub fn read_back(&self, texture: &ID3D11Texture2D) -> Result<Vec<u8>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let bytes_per_pixel = match desc.Format {
            DXGI_FORMAT_R16G16B16A16_FLOAT => 8,
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R32_UINT => 4,
            DXGI_FORMAT_R32_FLOAT => 4,
            other => bail!("read_back does not handle {other:?}"),
        };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
        }
        let staging = staging.context("CreateTexture2D returned no staging texture")?;
        unsafe {
            self.context.CopyResource(&staging, texture);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let row = desc.Width as usize * bytes_per_pixel;
            let mut bytes = vec![0u8; row * desc.Height as usize];
            for y in 0..desc.Height as usize {
                let src = std::slice::from_raw_parts(
                    map.pData.cast::<u8>().add(y * map.RowPitch as usize),
                    row,
                );
                bytes[y * row..(y + 1) * row].copy_from_slice(src);
            }
            self.context.Unmap(&staging, 0);
            Ok(bytes)
        }
    }
}

pub fn wide_to_string(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}

/// The compiled HDR/WCG → SDR shader passes.
pub struct SdrConverter {
    tile_peak: ID3D11ComputeShader,
    frame_peak: ID3D11ComputeShader,
    proximity: ID3D11ComputeShader,
    convert: ID3D11ComputeShader,
}

/// A converted frame.
pub struct SdrFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 sRGB, opaque.
    pub rgba: Vec<u8>,
    /// Peak of the frame relative to SDR white. Above 1 means HDR content.
    pub frame_peak: f32,
}

#[repr(C)]
struct Params {
    white_scale: f32,
    width: u32,
    height: u32,
    tiles_x: u32,
    tiles_y: u32,
    highlight_mode: u32,
    unused: [u32; 2],
}

impl SdrConverter {
    pub fn new(gpu: &Gpu) -> Result<Self> {
        Ok(Self {
            tile_peak: compile(gpu, s!("tile_peak"))?,
            frame_peak: compile(gpu, s!("frame_peak"))?,
            proximity: compile(gpu, s!("proximity"))?,
            convert: compile(gpu, s!("convert"))?,
        })
    }

    /// Convert an FP16 scRGB texture to 8-bit sRGB.
    pub fn convert(
        &self,
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        white_scale: f32,
        mode: Highlights,
    ) -> Result<SdrFrame> {
        ensure!(
            white_scale.is_finite() && white_scale > 0.0,
            "invalid SDR white scale {white_scale}"
        );
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        ensure!(
            desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT,
            "expected an FP16 scRGB texture, got {:?}",
            desc.Format
        );
        let (width, height) = (desc.Width, desc.Height);
        let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
        let read_write = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS;
        let peaks = gpu.texture(tiles_x, tiles_y, DXGI_FORMAT_R32_FLOAT, read_write, None)?;
        let peak = gpu.texture(1, 1, DXGI_FORMAT_R32_FLOAT, read_write, None)?;
        let near = gpu.texture(tiles_x, tiles_y, DXGI_FORMAT_R32_FLOAT, read_write, None)?;
        let output = gpu.texture(
            width,
            height,
            DXGI_FORMAT_R32_UINT,
            D3D11_BIND_UNORDERED_ACCESS,
            None,
        )?;
        let source_srv = srv(gpu, source)?;
        let params = Params {
            white_scale,
            width,
            height,
            tiles_x,
            tiles_y,
            highlight_mode: match mode {
                Highlights::Shoulder => 0,
                Highlights::Clip => 1,
            },
            unused: [0; 2],
        };
        let mut buffer = None;
        unsafe {
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: size_of::<Params>() as u32,
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                Some(&D3D11_SUBRESOURCE_DATA {
                    pSysMem: (&params as *const Params).cast(),
                    ..Default::default()
                }),
                Some(&mut buffer),
            )?;
        }

        // Each pass binds exactly what it reads and writes; everything else is
        // unbound, so no resource is ever an input and an output at once.
        let pass = |shader: &ID3D11ComputeShader,
                    inputs: [Option<ID3D11ShaderResourceView>; 3],
                    outputs: [Option<ID3D11UnorderedAccessView>; 2],
                    groups: (u32, u32)| unsafe {
            let ctx = &gpu.context;
            ctx.CSSetUnorderedAccessViews(0, 2, Some([None, None].as_ptr()), None);
            ctx.CSSetShaderResources(0, Some(&[None, None, None]));
            ctx.CSSetShader(shader, None);
            ctx.CSSetConstantBuffers(0, Some(&[buffer.clone()]));
            ctx.CSSetShaderResources(0, Some(&inputs));
            ctx.CSSetUnorderedAccessViews(0, 2, Some(outputs.as_ptr()), None);
            ctx.Dispatch(groups.0, groups.1, 1);
        };
        pass(
            &self.tile_peak,
            [Some(source_srv.clone()), None, None],
            [Some(uav(gpu, &peaks)?), None],
            (tiles_x, tiles_y),
        );
        pass(
            &self.frame_peak,
            [None, Some(srv(gpu, &peaks)?), None],
            [Some(uav(gpu, &peak)?), None],
            (1, 1),
        );
        pass(
            &self.proximity,
            [None, Some(srv(gpu, &peaks)?), None],
            [Some(uav(gpu, &near)?), None],
            (tiles_x.div_ceil(8), tiles_y.div_ceil(8)),
        );
        pass(
            &self.convert,
            [
                Some(source_srv),
                Some(srv(gpu, &near)?),
                Some(srv(gpu, &peak)?),
            ],
            [None, Some(uav(gpu, &output)?)],
            (width.div_ceil(8), height.div_ceil(8)),
        );
        unsafe {
            let ctx = &gpu.context;
            ctx.CSSetUnorderedAccessViews(0, 2, Some([None, None].as_ptr()), None);
            ctx.CSSetShaderResources(0, Some(&[None, None, None]));
            ctx.CSSetShader(None, None);
        }
        let peak_bytes = gpu.read_back(&peak)?;
        // R32_UINT packs r | g << 8 | b << 16 | a << 24, so the little-endian
        // bytes are already RGBA.
        Ok(SdrFrame {
            width,
            height,
            rgba: gpu.read_back(&output)?,
            frame_peak: f32::from_le_bytes(peak_bytes[..4].try_into()?),
        })
    }
}

fn srv(gpu: &Gpu, texture: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    unsafe {
        gpu.device
            .CreateShaderResourceView(texture, None, Some(&mut view))?
    };
    view.context("no shader resource view")
}

fn uav(gpu: &Gpu, texture: &ID3D11Texture2D) -> Result<ID3D11UnorderedAccessView> {
    let mut view = None;
    unsafe {
        gpu.device
            .CreateUnorderedAccessView(texture, None, Some(&mut view))?
    };
    view.context("no unordered access view")
}

fn compile(gpu: &Gpu, entry: PCSTR) -> Result<ID3D11ComputeShader> {
    let (mut code, mut errors) = (None, None);
    let result = unsafe {
        D3DCompile(
            SHADER_SOURCE.as_ptr().cast(),
            SHADER_SOURCE.len(),
            s!("hdr_to_sdr.hlsl"),
            None,
            None,
            entry,
            s!("cs_5_0"),
            D3DCOMPILE_ENABLE_STRICTNESS
                | D3DCOMPILE_IEEE_STRICTNESS
                | D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let detail = errors.map(|b| blob_text(&b)).unwrap_or_default();
        return Err(anyhow!(
            "shader {} failed to compile: {e}\n{detail}",
            unsafe { entry.display() }
        ));
    }
    let code = code.context("D3DCompile returned no bytecode")?;
    let mut shader = None;
    unsafe {
        let bytes =
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        gpu.device
            .CreateComputeShader(bytes, None, Some(&mut shader))?;
    }
    shader.context("CreateComputeShader returned no shader")
}

fn blob_text(blob: &ID3DBlob) -> String {
    unsafe {
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        String::from_utf8_lossy(bytes).into_owned()
    }
}
