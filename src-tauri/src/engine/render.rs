//! 渲染管线（spec §5）：抓取纹理 → D2D 特效链
//! （GaussianBlur → DisplacementMap → Saturation）→ 20px 圆角裁剪 → DComp 表面，
//! DComp 目标 topmost=FALSE，垫在 WebView 子窗口后面（Phase 0 已验证层序）。
//! 全部对象建在抓取通道的 D3D 设备上（同一适配器）。

use crate::engine::dispmap;
use windows::core::{IUnknown, Interface};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BORDER_MODE_HARD, D2D1_COLOR_F,
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Effect,
    ID2D1Factory1, ID2D1Geometry, ID2D1Image, ID2D1Layer, CLSID_D2D1DisplacementMap,
    CLSID_D2D1GaussianBlur, CLSID_D2D1Saturation, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_CHANNEL_SELECTOR_G, D2D1_CHANNEL_SELECTOR_R,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DISPLACEMENTMAP_PROP_SCALE,
    D2D1_DISPLACEMENTMAP_PROP_X_CHANNEL_SELECT, D2D1_DISPLACEMENTMAP_PROP_Y_CHANNEL_SELECT,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_PARAMETERS1, D2D1_LAYER_OPTIONS1_NONE,
    D2D1_MAP_OPTIONS_READ, D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT, D2D1_ROUNDED_RECT,
    D2D1_SATURATION_PROP_SATURATION,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDesktopDevice, IDCompositionSurface,
    IDCompositionTarget, IDCompositionVisual2,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows_numerics::{Matrix3x2, Vector2};

/// 玻璃参数，**已换算成物理像素**（saturate 无量纲）。
#[derive(Clone, Copy, Debug)]
pub struct GlassParams {
    pub sigma: f32,        // = blur/2 × dpr
    pub displacement: f32, // = displacement × dpr
    pub band: f32,         // × dpr
    pub radius: f32,       // × dpr
    pub margin: i32,       // × dpr
    pub saturate: f32,
}

fn e(ctx: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |err| format!("{ctx}: {err}")
}

pub struct Renderer {
    factory: ID2D1Factory1,
    d2d: ID2D1Device,
    ctx: ID2D1DeviceContext,
    dcomp: IDCompositionDesktopDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual2,
    surface: Option<IDCompositionSurface>,
    blur: ID2D1Effect,
    disp: ID2D1Effect,
    sat: ID2D1Effect,
    mask: Option<ID2D1Geometry>,
    src_bitmap: Option<(u64, ID2D1Bitmap1)>, // (抓取纹理代数, D2D 包装)
    win_w: u32,
    win_h: u32,
    params: GlassParams,
}

impl Renderer {
    /// device 必须是抓取通道的设备。
    pub fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        hwnd: isize,
    ) -> Result<Self, String> {
        unsafe {
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice =
                device.cast().map_err(e("cast IDXGIDevice"))?;
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                    .map_err(e("D2D1CreateFactory"))?;
            let d2d: ID2D1Device = factory
                .CreateDevice(&dxgi)
                .map_err(e("CreateDevice(d2d)"))?;
            let ctx = d2d
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .map_err(e("CreateDeviceContext"))?;

            let unk: IUnknown = d2d.cast().map_err(e("cast IUnknown"))?;
            let dcomp: IDCompositionDesktopDevice =
                DCompositionCreateDevice2(&unk).map_err(e("DCompositionCreateDevice2"))?;
            let target = dcomp
                .CreateTargetForHwnd(HWND(hwnd as *mut _), false)
                .map_err(e("CreateTargetForHwnd"))?;
            let visual = dcomp.CreateVisual().map_err(e("CreateVisual"))?;
            target.SetRoot(&visual).map_err(e("SetRoot"))?;

            let blur = ctx
                .CreateEffect(&CLSID_D2D1GaussianBlur)
                .map_err(e("blur effect"))?;
            let disp = ctx
                .CreateEffect(&CLSID_D2D1DisplacementMap)
                .map_err(e("disp effect"))?;
            let sat = ctx
                .CreateEffect(&CLSID_D2D1Saturation)
                .map_err(e("sat effect"))?;

            Ok(Self {
                factory,
                d2d,
                ctx,
                dcomp,
                _target: target,
                visual,
                surface: None,
                blur,
                disp,
                sat,
                mask: None,
                src_bitmap: None,
                win_w: 0,
                win_h: 0,
                params: GlassParams {
                    sigma: 7.0,
                    displacement: 24.0,
                    band: 16.0,
                    radius: 20.0,
                    margin: 24,
                    saturate: 1.12,
                },
            })
        }
    }

    /// 窗口物理尺寸或玻璃参数变化：重建表面、位移图、圆角、特效参数。
    pub fn set_geometry(&mut self, win_w: u32, win_h: u32, params: GlassParams) -> Result<(), String> {
        unsafe {
            self.win_w = win_w;
            self.win_h = win_h;
            self.params = params;
            let m = params.margin as u32;
            let (cw, ch) = (win_w + 2 * m, win_h + 2 * m);

            // DComp 表面 = 窗口大小
            let surface = self
                .dcomp
                .CreateSurface(
                    win_w,
                    win_h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
                .map_err(e("CreateSurface"))?;
            self.visual
                .SetContent(&surface)
                .map_err(e("SetContent"))?;
            self.surface = Some(surface);

            // 位移图：窗口±margin，玻璃矩形在 (m,m)
            let field = dispmap::disp_field(cw, ch, m, params.radius as f64, params.band as f64);
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let map_bmp = self
                .ctx
                .CreateBitmap(
                    D2D_SIZE_U { width: cw, height: ch },
                    Some(field.as_ptr() as *const _),
                    cw * 4,
                    &props,
                )
                .map_err(e("dispmap bitmap"))?;

            // 特效参数
            set_f32(&self.blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, params.sigma)?;
            set_enum(&self.blur, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32, D2D1_BORDER_MODE_HARD.0 as u32)?;
            set_f32(&self.disp, D2D1_DISPLACEMENTMAP_PROP_SCALE.0 as u32, params.displacement)?;
            set_enum(&self.disp, D2D1_DISPLACEMENTMAP_PROP_X_CHANNEL_SELECT.0 as u32, D2D1_CHANNEL_SELECTOR_R.0 as u32)?;
            set_enum(&self.disp, D2D1_DISPLACEMENTMAP_PROP_Y_CHANNEL_SELECT.0 as u32, D2D1_CHANNEL_SELECTOR_G.0 as u32)?;
            set_f32(&self.sat, D2D1_SATURATION_PROP_SATURATION.0 as u32, params.saturate)?;

            // 链接：blur → disp(输入1=位移图) → sat
            let blur_out = self.blur.GetOutput().map_err(e("blur out"))?;
            self.disp.SetInput(0, &blur_out, true);
            let map_img: ID2D1Image = map_bmp.cast().map_err(e("map cast"))?;
            self.disp.SetInput(1, &map_img, true);
            let disp_out = self.disp.GetOutput().map_err(e("disp out"))?;
            self.sat.SetInput(0, &disp_out, true);

            // 圆角掩模（窗口局部坐标，绘制时用 maskTransform 平移）
            let rounded = self
                .factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: win_w as f32,
                        bottom: win_h as f32,
                    },
                    radiusX: params.radius,
                    radiusY: params.radius,
                })
                .map_err(e("rounded geometry"))?;
            self.mask = Some(rounded.cast().map_err(e("geometry cast"))?);
            // 源位图对应的纹理没变，但特效输入图可能因重建被清 → 让 render 重新绑
            self.src_bitmap = None;
            Ok(())
        }
    }

    /// 把抓取通道的当前区域纹理跑一遍管线画到 DComp 表面并提交。
    pub fn render(&mut self, tex: &ID3D11Texture2D, generation: u64) -> Result<(), String> {
        unsafe {
            self.bind_source(tex, generation)?;
            let surface = self.surface.as_ref().ok_or("no surface")?;
            let mut off = windows::Win32::Foundation::POINT::default();
            let dc: ID2D1DeviceContext = surface
                .BeginDraw(None, &mut off)
                .map_err(e("surface BeginDraw"))?;
            let r = self.draw_scene(&dc, off.x as f32, off.y as f32);
            surface.EndDraw().map_err(e("surface EndDraw"))?;
            r?;
            self.dcomp.Commit().map_err(e("Commit"))?;
            Ok(())
        }
    }

    /// 同一场景离屏渲染并回读 BGRA（dump_glass / 验收用）。
    pub fn render_to_cpu(
        &mut self,
        tex: &ID3D11Texture2D,
        generation: u64,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        unsafe {
            self.bind_source(tex, generation)?;
            let (w, h) = (self.win_w, self.win_h);
            let mk = |opts, alpha| D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: alpha,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: opts,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let target = self
                .ctx
                .CreateBitmap(
                    D2D_SIZE_U { width: w, height: h },
                    None,
                    0,
                    &mk(D2D1_BITMAP_OPTIONS_TARGET, D2D1_ALPHA_MODE_PREMULTIPLIED),
                )
                .map_err(e("target bitmap"))?;
            self.ctx.SetTarget(&target);
            self.ctx.BeginDraw();
            let r = self.draw_scene(&self.ctx.clone(), 0.0, 0.0);
            self.ctx.EndDraw(None, None).map_err(e("EndDraw"))?;
            self.ctx.SetTarget(None::<&ID2D1Image>);
            r?;

            let cpu = self
                .ctx
                .CreateBitmap(
                    D2D_SIZE_U { width: w, height: h },
                    None,
                    0,
                    &mk(
                        D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                        D2D1_ALPHA_MODE_PREMULTIPLIED,
                    ),
                )
                .map_err(e("cpu bitmap"))?;
            cpu.CopyFromBitmap(None, &target, None)
                .map_err(e("CopyFromBitmap"))?;
            let mapped = cpu.Map(D2D1_MAP_OPTIONS_READ).map_err(e("Map"))?;
            let mut bytes = vec![0u8; (w * h * 4) as usize];
            for row in 0..h {
                let src = (mapped.bits as *const u8).add((row * mapped.pitch) as usize);
                std::ptr::copy_nonoverlapping(
                    src,
                    bytes.as_mut_ptr().add((row * w * 4) as usize),
                    (w * 4) as usize,
                );
            }
            cpu.Unmap().map_err(e("Unmap"))?;
            for px in bytes.chunks_exact_mut(4) {
                px[3] = 255;
            }
            Ok((w, h, bytes))
        }
    }

    /// 抓取纹理 → D2D 位图包装（按代数缓存），设为 blur 输入。
    fn bind_source(&mut self, tex: &ID3D11Texture2D, generation: u64) -> Result<(), String> {
        unsafe {
            let stale = !matches!(&self.src_bitmap, Some((g, _)) if *g == generation);
            if stale {
                let dxgi_surf: IDXGISurface = tex.cast().map_err(e("tex→IDXGISurface"))?;
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        // 桌面帧 alpha 未定义：忽略，当不透明
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = self
                    .ctx
                    .CreateBitmapFromDxgiSurface(&dxgi_surf, Some(&props))
                    .map_err(e("CreateBitmapFromDxgiSurface"))?;
                let bmp_img: ID2D1Image = bmp.cast().map_err(e("bmp cast"))?;
                self.blur.SetInput(0, &bmp_img, true);
                self.src_bitmap = Some((generation, bmp));
            }
            Ok(())
        }
    }

    /// 场景：清透明 → 圆角层 → 画特效输出（源图原点在窗口原点左上 −margin）。
    fn draw_scene(&self, dc: &ID2D1DeviceContext, ox: f32, oy: f32) -> Result<(), String> {
        unsafe {
            dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            let mask = self.mask.as_ref().ok_or("no mask")?;
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: D2D_RECT_F {
                    left: -1.0e9,
                    top: -1.0e9,
                    right: 1.0e9,
                    bottom: 1.0e9,
                },
                geometricMask: std::mem::ManuallyDrop::new(Some(mask.clone())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::translation(ox, oy),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            dc.PushLayer(&params, None::<&ID2D1Layer>);
            let out = self.sat.GetOutput().map_err(e("sat out"))?;
            let m = self.params.margin as f32;
            dc.DrawImage(
                &out,
                Some(&Vector2 {
                    X: ox - m,
                    Y: oy - m,
                }),
                None,
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
            dc.PopLayer();
            // ManuallyDrop 里的接口克隆需要手动放掉
            let mut p = params;
            std::mem::ManuallyDrop::drop(&mut p.geometricMask);
            std::mem::ManuallyDrop::drop(&mut p.opacityBrush);
            Ok(())
        }
    }
}

fn set_f32(effect: &ID2D1Effect, index: u32, v: f32) -> Result<(), String> {
    unsafe {
        effect
            .SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &v.to_le_bytes())
            .map_err(|err| format!("SetValue f32 #{index}: {err}"))
    }
}

fn set_enum(effect: &ID2D1Effect, index: u32, v: u32) -> Result<(), String> {
    unsafe {
        effect
            .SetValue(index, D2D1_PROPERTY_TYPE_ENUM, &v.to_le_bytes())
            .map_err(|err| format!("SetValue enum #{index}: {err}"))
    }
}
