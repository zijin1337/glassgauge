//! 位移图（spec §5）：`ui/disp.js` 的 Rust 镜像，**字节序为 BGRA**（D2D 位图默认）。
//! 数学与 disp.js 完全一致；单测逐条镜像 ui/tests/disp.test.js —— 两边同改。
//! R 通道编码 dx、G 通道编码 dy，128 为中性；只有玻璃边缘向内 band 像素有位移。

/// 画布 W×H，玻璃矩形内缩 m，圆角 r，边带 band。返回 BGRA 字节。
pub fn disp_field(cw: u32, ch: u32, m: u32, r: f64, band: f64) -> Vec<u8> {
    let (cw_i, ch_i, m_i) = (cw as i32, ch as i32, m as i32);
    let w = (cw_i - 2 * m_i) as f64;
    let h = (ch_i - 2 * m_i) as f64;
    let hw = w / 2.0;
    let hh = h / 2.0;
    let rr = r.min(hw).min(hh);
    let mut data = vec![0u8; (cw * ch * 4) as usize];
    for y in 0..ch_i {
        for x in 0..cw_i {
            let i = ((y * cw_i + x) * 4) as usize;
            data[i] = 128; // B
            data[i + 1] = 128; // G
            data[i + 2] = 128; // R
            data[i + 3] = 255; // A
            let px = (x - m_i) as f64;
            let py = (y - m_i) as f64;
            let qx = (px - hw).abs() - (hw - rr);
            let qy = (py - hh).abs() - (hh - rr);
            let mx = qx.max(0.0);
            let my = qy.max(0.0);
            let d = mx.hypot(my) - rr; // 边界处 0，玻璃内为负
            if d > 0.0 {
                continue; // 玻璃外：中性
            }
            let t = (1.0 + d / band).clamp(0.0, 1.0);
            if t == 0.0 {
                continue; // 深处：中性
            }
            let e = t * t * (3.0 - 2.0 * t); // smoothstep
            let (mut nx, mut ny);
            if mx > 0.0 || my > 0.0 {
                let l = mx.hypot(my).max(1.0e-9);
                nx = mx / l;
                ny = my / l;
            } else if qx > qy {
                nx = 1.0;
                ny = 0.0;
            } else {
                nx = 0.0;
                ny = 1.0;
            }
            nx *= sign_or_one(px - hw);
            ny *= sign_or_one(py - hh);
            data[i + 2] = quantize(128.0 + 127.0 * nx * e); // R = dx
            data[i + 1] = quantize(128.0 + 127.0 * ny * e); // G = dy
        }
    }
    data
}

/// JS 的 `Math.sign(v) || 1`：0 时取 1。
fn sign_or_one(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// Uint8ClampedArray 语义：clamp 后四舍五入。
fn quantize(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    // 与 ui/tests/disp.test.js 相同的基准：玻璃 100×60，外扩 24，圆角 20，边带 16
    const M: u32 = 24;
    const GW: u32 = 100;
    const GH: u32 = 60;
    const CW: u32 = GW + 2 * M; // 148
    const CH: u32 = GH + 2 * M; // 108

    fn field() -> Vec<u8> {
        disp_field(CW, CH, M, 20.0, 16.0)
    }

    /// 返回 (R, G, A) —— 注意存储是 BGRA。
    fn px(f: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
        let i = ((y * CW + x) * 4) as usize;
        (f[i + 2], f[i + 1], f[i + 3])
    }

    #[test]
    fn margin_area_is_neutral() {
        let f = field();
        for (x, y) in [(0, 0), (CW - 1, CH - 1), (12, 54), (CW - 5, 3)] {
            let (r, g, _) = px(&f, x, y);
            assert_eq!((r, g), (128, 128), "at ({x},{y})");
        }
    }

    #[test]
    fn deep_center_is_neutral() {
        let f = field();
        assert_eq!(px(&f, M + GW / 2, M + GH / 2), (128, 128, 255));
    }

    #[test]
    fn band_directions() {
        let f = field();
        let mid_y = M + GH / 2;
        let mid_x = M + GW / 2;
        assert!(px(&f, M + 1, mid_y).0 < 128, "左边带 R<128");
        assert!(px(&f, M + GW - 2, mid_y).0 > 128, "右边带 R>128");
        assert!(px(&f, mid_x, M + GH - 2).1 > 128, "下边带 G>128");
        assert!(px(&f, mid_x, M + 1).1 < 128, "上边带 G<128");
    }

    #[test]
    fn corner_diagonal_and_outside_arc() {
        let f = field();
        let (r, g, _) = px(&f, M + GW - 8, M + GH - 8); // 弧内侧
        assert!(r > 128 && g > 128);
        let (ro, go, _) = px(&f, M + GW - 4, M + GH - 4); // 弧外被裁区
        assert_eq!((ro, go), (128, 128));
    }

    #[test]
    fn alpha_is_opaque_everywhere() {
        let f = field();
        assert!(f.chunks_exact(4).all(|p| p[3] == 255));
    }
}
