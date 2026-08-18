//! 纯几何（spec §7）：选屏与抓取区计算。不碰 COM，全部可单测。
//! 坐标一律是虚拟桌面物理像素；输出局部坐标只在 crop_region 的返回值里出现。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// 窗口中心所在输出的下标。中心掉出所有屏（理论上不该发生）→ 第一个输出。
pub fn pick_output(outputs: &[Rect], cx: i32, cy: i32) -> Option<usize> {
    if outputs.is_empty() {
        return None;
    }
    Some(
        outputs
            .iter()
            .position(|o| o.contains(cx, cy))
            .unwrap_or(0),
    )
}

/// 抓取区 = 窗口±margin，clamp 到输出边界，返回**输出局部坐标**。
/// 窗口与该输出完全不相交 → None。
pub fn crop_region(win: Rect, margin: i32, output: Rect) -> Option<Rect> {
    let left = (win.left - margin).max(output.left);
    let top = (win.top - margin).max(output.top);
    let right = (win.right + margin).min(output.right);
    let bottom = (win.bottom + margin).min(output.bottom);
    if left >= right || top >= bottom {
        return None;
    }
    Some(Rect::new(
        left - output.left,
        top - output.top,
        right - output.left,
        bottom - output.top,
    ))
}

/// 抓取区相对窗口的偏移（渲染时把窗口区对齐到表面原点用）：
/// 窗口左上角在抓取区（输出局部）里的位置。
pub fn window_offset_in_crop(win: Rect, output: Rect, crop: Rect) -> (i32, i32) {
    (
        (win.left - output.left) - crop.left,
        (win.top - output.top) - crop.top,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机双屏基准（spec §7）：主屏 2560×1440 @ (0,0)，副屏竖 1280×2048 @ (-1280,0)。
    fn outputs() -> Vec<Rect> {
        vec![
            Rect::new(0, 0, 2560, 1440),
            Rect::new(-1280, 0, 0, 2048),
        ]
    }

    #[test]
    fn picks_output_by_window_center() {
        let o = outputs();
        assert_eq!(pick_output(&o, 1805, 203), Some(0)); // 主屏内
        assert_eq!(pick_output(&o, -600, 900), Some(1)); // 副屏内
        assert_eq!(pick_output(&o, 9999, 9999), Some(0)); // 掉出所有屏 → 第一个
        assert_eq!(pick_output(&[], 0, 0), None);
    }

    #[test]
    fn crop_is_window_plus_margin_in_output_local_coords() {
        let o = outputs();
        // 实测窗口：(1653,164) 305×78，margin 24
        let win = Rect::new(1653, 164, 1653 + 305, 164 + 78);
        let c = crop_region(win, 24, o[0]).unwrap();
        assert_eq!(c, Rect::new(1629, 140, 1982, 266));
        assert_eq!((c.width(), c.height()), (353, 126));
        assert_eq!(window_offset_in_crop(win, o[0], c), (24, 24));
    }

    #[test]
    fn crop_clamps_at_output_edges_and_offset_shrinks() {
        let o = outputs();
        // 窗口贴主屏左上角：margin 被 clamp 掉
        let win = Rect::new(4, 6, 4 + 305, 6 + 78);
        let c = crop_region(win, 24, o[0]).unwrap();
        assert_eq!(c, Rect::new(0, 0, 333, 108));
        assert_eq!(window_offset_in_crop(win, o[0], c), (4, 6));
    }

    #[test]
    fn crop_on_secondary_is_local_to_it() {
        let o = outputs();
        let win = Rect::new(-700, 900, -700 + 305, 900 + 78);
        let c = crop_region(win, 24, o[1]).unwrap();
        // 局部坐标：-700 − (−1280) − 24 = 556
        assert_eq!(c, Rect::new(556, 876, 909, 1002));
    }

    #[test]
    fn no_intersection_returns_none() {
        let o = outputs();
        let win = Rect::new(1653, 164, 1653 + 305, 164 + 78);
        assert_eq!(crop_region(win, 24, o[1]), None); // 完全在主屏 → 与副屏不交
    }
}
