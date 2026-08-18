//! Phase 0 技术验证（plan §Phase0）。`GG_SPIKE=b|a` 时从 setup 调用。
//! b = 层序：DComp 目标(topmost=FALSE) 的半透明色是否垫在 WebView 内容后面；
//! a = 剔除：设 WDA_EXCLUDEFROMCAPTURE 后，桌面复制帧里是否已无挂件自己。
//! 验证通过后，设备创建/WIC 落盘等工具函数会被后续阶段复用。

use std::os::windows::ffi::OsStrExt;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11DeviceContext1, ID3D11Resource,
    ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput,
    IDXGIOutput1, IDXGIResource, IDXGISurface, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};

pub fn run(which: &str, hwnd: isize, w: u32, h: u32, x: i32, y: i32) {
    let res = match which {
        "b" => spike_b(hwnd, w, h),
        "a" => spike_a(hwnd, x, y, w, h),
        "cap" => spike_cap(hwnd, x, y, w, h),
        other => Err(format!("unknown spike '{other}'")),
    };
    let msg = match &res {
        Ok(m) => format!("OK: {m}"),
        Err(e) => format!("FAIL: {e}"),
    };
    eprintln!("[spike {which}] {msg}");
    let _ = std::fs::write(
        crate::window::appdata_dir().join(format!("spike-{which}.txt")),
        &msg,
    );
}

fn e(ctx: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |err| format!("{ctx}: {err}")
}

/* ---------- spike B：DComp 垫在 WebView 后面 ---------- */

fn spike_b(hwnd: isize, w: u32, h: u32) -> Result<String, String> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .map_err(e("D3D11CreateDevice"))?;
        let device = device.ok_or("no d3d11 device")?;
        let dxgi: IDXGIDevice = device.cast().map_err(e("cast IDXGIDevice"))?;
        let dcomp: IDCompositionDevice =
            DCompositionCreateDevice(&dxgi).map_err(e("DCompositionCreateDevice"))?;
        // topmost=FALSE：视觉树画在所有子窗口（= WebView2）后面
        let target = dcomp
            .CreateTargetForHwnd(HWND(hwnd as *mut _), false)
            .map_err(e("CreateTargetForHwnd"))?;
        let visual = dcomp.CreateVisual().map_err(e("CreateVisual"))?;
        let surface = dcomp
            .CreateSurface(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED)
            .map_err(e("CreateSurface"))?;

        let mut off = POINT::default();
        let dxsurf: IDXGISurface = surface.BeginDraw(None, &mut off).map_err(e("BeginDraw"))?;
        let res2d: ID3D11Resource = dxsurf.cast().map_err(e("cast ID3D11Resource"))?;
        let mut rtv = None;
        device
            .CreateRenderTargetView(&res2d, None, Some(&mut rtv))
            .map_err(e("CreateRenderTargetView"))?;
        let rtv = rtv.ok_or("no rtv")?;
        let ctx: ID3D11DeviceContext = device.GetImmediateContext().map_err(e("ctx"))?;
        // 表面可能是图集的一块：只清 BeginDraw 给的偏移矩形，别越界
        let ctx1: ID3D11DeviceContext1 = ctx.cast().map_err(e("cast ctx1"))?;
        let rect = RECT {
            left: off.x,
            top: off.y,
            right: off.x + w as i32,
            bottom: off.y + h as i32,
        };
        // 半透明蓝（premultiplied：rgb 已乘 alpha）——肉眼一眼能认
        ctx1.ClearView(&rtv, &[0.055f32, 0.165, 0.36, 0.55], Some(&[rect]));
        surface.EndDraw().map_err(e("EndDraw"))?;

        visual.SetContent(&surface).map_err(e("SetContent"))?;
        target.SetRoot(&visual).map_err(e("SetRoot"))?;
        dcomp.Commit().map_err(e("Commit"))?;
        // 合成对象析构画面即消失：spike 期间保活到进程退出
        std::mem::forget((device, dxgi, dcomp, target, visual, surface));
        Ok(format!("committed {w}x{h} translucent blue sheet (topmost=FALSE)"))
    }
}

/* ---------- spike A：剔除自己后抓帧落盘 ---------- */

fn spike_a(hwnd: isize, x: i32, y: i32, w: u32, h: u32) -> Result<String, String> {
    unsafe {
        SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE)
            .map_err(e("SetWindowDisplayAffinity"))?;
        std::thread::sleep(std::time::Duration::from_millis(300)); // 等 DWM 重合成

        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(e("CreateDXGIFactory1"))?;
        let (cx, cy) = (x + w as i32 / 2, y + h as i32 / 2);
        let mut found: Option<(IDXGIAdapter1, IDXGIOutput, RECT)> = None;
        let mut ai = 0u32;
        'outer: while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0u32;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                let desc = output.GetDesc().map_err(e("GetDesc"))?;
                let r = desc.DesktopCoordinates;
                if cx >= r.left && cx < r.right && cy >= r.top && cy < r.bottom {
                    found = Some((adapter, output, r));
                    break 'outer;
                }
                oi += 1;
            }
            ai += 1;
        }
        let (adapter, output, orect) = found.ok_or("no output contains window center")?;

        // 设备必须建在输出所属适配器上（混合显卡预案，plan 风险表）
        let adapter0: IDXGIAdapter = adapter.cast().map_err(e("cast IDXGIAdapter"))?;
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            &adapter0,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .map_err(e("D3D11CreateDevice(adapter)"))?;
        let device = device.ok_or("no d3d11 device")?;
        let ctx: ID3D11DeviceContext = device.GetImmediateContext().map_err(e("ctx"))?;
        let out1: IDXGIOutput1 = output.cast().map_err(e("cast IDXGIOutput1"))?;
        let dup = out1.DuplicateOutput(&device).map_err(e("DuplicateOutput"))?;

        // 首帧行为因驱动而异：记录元数据，优先取有累积内容的帧，
        // 全零帧（部分驱动首帧黑）跳过重试。
        let mut staging: Option<ID3D11Texture2D> = None;
        let mut diag = String::new();
        for i in 0..40 {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match dup.AcquireNextFrame(250, &mut info, &mut res) {
                Ok(()) => {
                    diag.push_str(&format!(
                        "#{i} present={} acc={} res={} | ",
                        info.LastPresentTime,
                        info.AccumulatedFrames,
                        res.is_some()
                    ));
                    if let Some(r) = res {
                        let tex: ID3D11Texture2D = r.cast().map_err(e("cast tex"))?;
                        let mut d = D3D11_TEXTURE2D_DESC::default();
                        tex.GetDesc(&mut d);
                        d.Usage = D3D11_USAGE_STAGING;
                        d.BindFlags = 0;
                        d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                        d.MiscFlags = 0;
                        let mut copy: Option<ID3D11Texture2D> = None;
                        device
                            .CreateTexture2D(&d, None, Some(&mut copy))
                            .map_err(e("CreateTexture2D staging"))?;
                        let copy = copy.ok_or("no staging")?;
                        ctx.CopyResource(&copy, &tex);
                        // 探针：全屏 4 点任意非零即认为拿到真内容
                        let probes = probe_pixels(&ctx, &copy, &d)?;
                        diag.push_str(&format!("probes={probes:?} | "));
                        staging = Some(copy);
                        let _ = dup.ReleaseFrame();
                        if probes.iter().any(|&v| v != 0) {
                            break; // 有真内容
                        }
                        continue; // 全零帧：留作兜底，继续等好帧
                    }
                    let _ = dup.ReleaseFrame();
                }
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    diag.push_str(&format!("#{i} timeout | "));
                    continue;
                }
                Err(err) => return Err(format!("AcquireNextFrame: {err} | diag: {diag}")),
            }
        }
        eprintln!("[spike a] {diag}");
        let _ = std::fs::write(crate::window::appdata_dir().join("spike-a-diag.txt"), &diag);
        let full = staging.ok_or_else(|| format!("no frame within 10s | diag: {diag}"))?;

        // 挂件±24 的输出局部区域
        let m = 24i32;
        let l = (x - orect.left - m).max(0);
        let t = (y - orect.top - m).max(0);
        let rr = (x - orect.left + w as i32 + m).min(orect.right - orect.left);
        let bb = (y - orect.top + h as i32 + m).min(orect.bottom - orect.top);
        let (rw, rh) = ((rr - l) as u32, (bb - t) as u32);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&full, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(e("Map"))?;
        let mut bytes = vec![0u8; (rw * rh * 4) as usize];
        for row in 0..rh {
            let src = (mapped.pData as *const u8)
                .add(((t as u32 + row) * mapped.RowPitch + l as u32 * 4) as usize);
            std::ptr::copy_nonoverlapping(
                src,
                bytes.as_mut_ptr().add((row * rw * 4) as usize),
                (rw * 4) as usize,
            );
        }
        ctx.Unmap(&full, 0);
        // 复制帧的 alpha 未定义：强制不透明，免得 PNG 里内容被透明掩掉
        for px in bytes.chunks_exact_mut(4) {
            px[3] = 255;
        }

        let path = crate::window::appdata_dir().join("spike-a.png");
        write_png_bgra(&path, rw, rh, &bytes)?;
        Ok(format!("dumped {rw}x{rh} region to {}", path.display()))
    }
}

/* ---------- spike cap：抓取通道验收（Phase 2） ---------- */

fn spike_cap(hwnd: isize, x: i32, y: i32, w: u32, h: u32) -> Result<String, String> {
    use crate::engine::{capture, geometry};
    unsafe {
        SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE)
            .map_err(e("SetWindowDisplayAffinity"))?;
    }
    let outputs = capture::list_outputs()?;
    let rects: Vec<_> = outputs.iter().map(|o| o.rect).collect();
    let idx = geometry::pick_output(&rects, x + w as i32 / 2, y + h as i32 / 2)
        .ok_or("no outputs")?;
    let mut ch = capture::Channel::new(outputs[idx])?;
    let win = geometry::Rect::new(x, y, x + w as i32, y + h as i32);
    let region =
        geometry::crop_region(win, 24, ch.output_rect).ok_or("window outside output")?;
    ch.set_region(region)?;

    let mut dumped = 0u32;
    let mut polls = 0u32;
    while dumped < 3 && polls < 200 {
        polls += 1;
        match ch.poll(100, false) {
            capture::Poll::Updated => {
                let (rw, rh, bytes) = ch.readback()?;
                let path = crate::window::appdata_dir().join(format!("spike-cap-{dumped}.png"));
                write_png_bgra(&path, rw, rh, &bytes)?;
                dumped += 1;
            }
            capture::Poll::NoChange => {}
            capture::Poll::Lost(err) => return Err(format!("channel lost: {err}")),
        }
    }
    Ok(format!("dumped {dumped} region frames in {polls} polls"))
}

/// 读 staging 纹理上分散的 4 个探针点（每点取 BGR 之和），判断是否全零帧。
unsafe fn probe_pixels(
    ctx: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    desc: &D3D11_TEXTURE2D_DESC,
) -> Result<[u32; 4], String> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .map_err(e("probe Map"))?;
    let (w, h) = (desc.Width, desc.Height);
    let pts = [
        (w / 2, h / 2),
        (w / 4, h / 4),
        (w * 3 / 4, h * 3 / 4),
        (w / 2, h - 8),
    ];
    let mut out = [0u32; 4];
    for (k, (px, py)) in pts.iter().enumerate() {
        let p = (mapped.pData as *const u8).add((py * mapped.RowPitch + px * 4) as usize);
        out[k] = *p as u32 + *p.add(1) as u32 + *p.add(2) as u32;
    }
    ctx.Unmap(staging, 0);
    Ok(out)
}

/* ---------- WIC PNG 落盘（后续 dump_glass 复用） ---------- */

pub(crate) fn write_png_bgra(
    path: &std::path::Path,
    w: u32,
    h: u32,
    bgra: &[u8],
) -> Result<(), String> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(e("WIC factory"))?;
        let stream = factory.CreateStream().map_err(e("CreateStream"))?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        stream
            .InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)
            .map_err(e("InitializeFromFilename"))?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .map_err(e("CreateEncoder"))?;
        encoder
            .Initialize(&stream, WICBitmapEncoderNoCache)
            .map_err(e("encoder Initialize"))?;
        let mut frame = None;
        let mut bag = None;
        encoder
            .CreateNewFrame(&mut frame, &mut bag)
            .map_err(e("CreateNewFrame"))?;
        let frame = frame.ok_or("no frame encode")?;
        frame.Initialize(bag.as_ref()).map_err(e("frame Initialize"))?;
        frame.SetSize(w, h).map_err(e("SetSize"))?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).map_err(e("SetPixelFormat"))?;
        frame
            .WritePixels(h, w * 4, bgra)
            .map_err(e("WritePixels"))?;
        frame.Commit().map_err(e("frame Commit"))?;
        encoder.Commit().map_err(e("encoder Commit"))?;
        Ok(())
    }
}
