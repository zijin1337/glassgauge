//! 抓取通道（spec §5/§6）：一个输出一条复制通道，维护一块**常驻区域纹理**。
//! Phase 0 实证的怪癖都在这处理：
//! - 首帧可能全零（LastPresentTime=0 / AccumulatedFrames=0）→ 未拿到累积帧前不算有内容；
//! - 指针移动帧不带图像更新 → 快速跳过；
//! - 脏区/移动区都不碰抓取区的帧 → 丢弃（静止时零渲染）。
//! 设备建在输出所属适配器上（混合显卡预案）；D2D/DComp 共用本通道的设备。

use crate::engine::geometry::Rect;
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT,
};

/// 输出快照（纯数据，枚举后即弃 COM 对象）。
#[derive(Clone, Copy, Debug)]
pub struct OutputInfo {
    pub adapter: u32,
    pub output: u32,
    pub rect: Rect,
}

pub fn list_outputs() -> Result<Vec<OutputInfo>, String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
        let mut outs = Vec::new();
        let mut ai = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0u32;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if let Ok(desc) = output.GetDesc() {
                    let r = desc.DesktopCoordinates;
                    outs.push(OutputInfo {
                        adapter: ai,
                        output: oi,
                        rect: Rect::new(r.left, r.top, r.right, r.bottom),
                    });
                }
                oi += 1;
            }
            ai += 1;
        }
        if outs.is_empty() {
            Err("no dxgi outputs".into())
        } else {
            Ok(outs)
        }
    }
}

pub enum Poll {
    /// 区域纹理有了新内容
    Updated,
    /// 无变化（超时/指针帧/脏区不相交/首帧尚无内容）
    NoChange,
    /// 通道失效（ACCESS_LOST 等），需要重建 → 状态机进 Degraded
    Lost(String),
}

pub struct Channel {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// 该输出的桌面坐标
    pub output_rect: Rect,
    /// 抓取区（输出局部坐标）
    region: Rect,
    region_tex: Option<ID3D11Texture2D>,
    has_content: bool,
}

impl Channel {
    pub fn new(info: OutputInfo) -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| format!("factory: {e}"))?;
            let adapter1 = factory
                .EnumAdapters1(info.adapter)
                .map_err(|e| format!("adapter {}: {e}", info.adapter))?;
            let output = adapter1
                .EnumOutputs(info.output)
                .map_err(|e| format!("output {}: {e}", info.output))?;
            let adapter: IDXGIAdapter = adapter1.cast().map_err(|e| format!("cast: {e}"))?;
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            let device = device.ok_or("no d3d11 device")?;
            let ctx = device
                .GetImmediateContext()
                .map_err(|e| format!("ctx: {e}"))?;
            let out1: IDXGIOutput1 = output.cast().map_err(|e| format!("IDXGIOutput1: {e}"))?;
            let dup = out1
                .DuplicateOutput(&device)
                .map_err(|e| format!("DuplicateOutput: {e}"))?;
            Ok(Self {
                device,
                ctx,
                dup,
                output_rect: info.rect,
                region: Rect::new(0, 0, 0, 0),
                region_tex: None,
                has_content: false,
            })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }
    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.ctx
    }
    pub fn region(&self) -> Rect {
        self.region
    }
    pub fn region_texture(&self) -> Option<&ID3D11Texture2D> {
        if self.has_content {
            self.region_tex.as_ref()
        } else {
            None
        }
    }

    /// 设定/变更抓取区（输出局部坐标）。尺寸变了重建常驻纹理。
    pub fn set_region(&mut self, region: Rect) -> Result<(), String> {
        let size_changed = region.width() != self.region.width()
            || region.height() != self.region.height()
            || self.region_tex.is_none();
        let moved = region != self.region;
        self.region = region;
        if size_changed {
            unsafe {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: region.width() as u32,
                    Height: region.height() as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut tex = None;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut tex))
                    .map_err(|e| format!("region tex: {e}"))?;
                self.region_tex = tex;
            }
            self.has_content = false;
        }
        if moved {
            // 位置变了但尺寸没变：纹理还在，内容已经对不上位 → 下一帧强制重拷
            self.has_content = false;
        }
        Ok(())
    }

    /// 取一帧。`force` = 即使脏区不相交也重拷（几何刚变过）。
    pub fn poll(&mut self, timeout_ms: u32, force: bool) -> Poll {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) {
                Ok(()) => {
                    let outcome = (|| -> Result<Poll, String> {
                        // 指针帧不带图像；但没拿到过内容/被强制时仍要试拷
                        if info.AccumulatedFrames == 0 && self.has_content && !force {
                            return Ok(Poll::NoChange);
                        }
                        let Some(r) = res.as_ref() else {
                            return Ok(Poll::NoChange);
                        };
                        if info.AccumulatedFrames == 0 && !self.has_content {
                            // 驱动首帧全零：跳过，等累积帧
                            return Ok(Poll::NoChange);
                        }
                        if self.has_content && !force && !self.frame_touches_region(&info) {
                            return Ok(Poll::NoChange);
                        }
                        let tex: ID3D11Texture2D =
                            r.cast().map_err(|e| format!("cast frame: {e}"))?;
                        self.copy_region_from(&tex)?;
                        self.has_content = true;
                        Ok(Poll::Updated)
                    })();
                    let _ = self.dup.ReleaseFrame();
                    outcome.unwrap_or_else(|e| Poll::Lost(e))
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => Poll::NoChange,
                Err(e) => Poll::Lost(format!("AcquireNextFrame: {e}")),
            }
        }
    }

    /// 本帧的脏区/移动区是否碰到抓取区。元数据读取失败按"碰到"处理（保守）。
    fn frame_touches_region(&self, info: &DXGI_OUTDUPL_FRAME_INFO) -> bool {
        if info.TotalMetadataBufferSize == 0 {
            return false;
        }
        unsafe {
            let mut moves = vec![DXGI_OUTDUPL_MOVE_RECT::default(); 64];
            let mut moves_size = 0u32;
            let move_bytes = (moves.len() * std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32;
            if self
                .dup
                .GetFrameMoveRects(move_bytes, moves.as_mut_ptr(), &mut moves_size)
                .is_err()
            {
                return true;
            }
            let n_moves = moves_size as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
            for mv in &moves[..n_moves] {
                let d = mv.DestinationRect;
                if self.intersects(Rect::new(d.left, d.top, d.right, d.bottom)) {
                    return true;
                }
            }
            let mut dirty = vec![windows::Win32::Foundation::RECT::default(); 128];
            let mut dirty_size = 0u32;
            let dirty_bytes =
                (dirty.len() * std::mem::size_of::<windows::Win32::Foundation::RECT>()) as u32;
            if self
                .dup
                .GetFrameDirtyRects(dirty_bytes, dirty.as_mut_ptr(), &mut dirty_size)
                .is_err()
            {
                return true;
            }
            let n_dirty =
                dirty_size as usize / std::mem::size_of::<windows::Win32::Foundation::RECT>();
            for d in &dirty[..n_dirty] {
                if self.intersects(Rect::new(d.left, d.top, d.right, d.bottom)) {
                    return true;
                }
            }
            false
        }
    }

    fn intersects(&self, r: Rect) -> bool {
        r.left < self.region.right
            && r.right > self.region.left
            && r.top < self.region.bottom
            && r.bottom > self.region.top
    }

    fn copy_region_from(&self, desktop: &ID3D11Texture2D) -> Result<(), String> {
        let tex = self.region_tex.as_ref().ok_or("no region tex")?;
        unsafe {
            let src = D3D11_BOX {
                left: self.region.left as u32,
                top: self.region.top as u32,
                front: 0,
                right: self.region.right as u32,
                bottom: self.region.bottom as u32,
                back: 1,
            };
            self.ctx
                .CopySubresourceRegion(tex, 0, 0, 0, 0, desktop, 0, Some(&src));
        }
        Ok(())
    }

    /// CPU 回读区域纹理（BGRA、alpha 置 255）。dump/验收用，热路径不走这里。
    pub fn readback(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let tex = self
            .region_texture()
            .ok_or("region texture has no content yet")?;
        read_texture_bgra(&self.device, &self.ctx, tex)
    }
}

/// 任意 BGRA 纹理 → CPU 字节（staging + Map），alpha 强制不透明。
pub fn read_texture_bgra(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let mut d = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut d);
        let (w, h) = (d.Width, d.Height);
        d.Usage = D3D11_USAGE_STAGING;
        d.BindFlags = 0;
        d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        d.MiscFlags = 0;
        let mut staging = None;
        device
            .CreateTexture2D(&d, None, Some(&mut staging))
            .map_err(|e| format!("staging: {e}"))?;
        let staging = staging.ok_or("no staging")?;
        ctx.CopyResource(&staging, tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| format!("map: {e}"))?;
        let mut bytes = vec![0u8; (w * h * 4) as usize];
        for row in 0..h {
            let src = (mapped.pData as *const u8).add((row * mapped.RowPitch) as usize);
            std::ptr::copy_nonoverlapping(
                src,
                bytes.as_mut_ptr().add((row * w * 4) as usize),
                (w * 4) as usize,
            );
        }
        ctx.Unmap(&staging, 0);
        for px in bytes.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok((w, h, bytes))
    }
}
