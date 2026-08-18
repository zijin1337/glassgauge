// 位移图纯计算（spec §6.2）：圆角矩形边带的法线场。
// R/G 编码 dx/dy，128 为中性；只有玻璃边缘向内 band 像素的一圈有位移。
// 画布比玻璃矩形四周各大 m 像素（模糊采样余量）；玻璃外一律中性，反正被圆角裁掉。

/** 返回 { data: Uint8ClampedArray, width, height }，可直接喂 ImageData。 */
export function dispField(W, H, m, r, band) {
  const w = W - 2 * m, h = H - 2 * m; // 玻璃矩形
  const hw = w / 2, hh = h / 2, rr = Math.min(r, hw, hh);
  const data = new Uint8ClampedArray(W * H * 4);
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const i = (y * W + x) * 4;
      data[i] = 128; data[i + 1] = 128; data[i + 2] = 128; data[i + 3] = 255;
      const px = x - m, py = y - m; // 玻璃局部坐标
      const qx = Math.abs(px - hw) - (hw - rr);
      const qy = Math.abs(py - hh) - (hh - rr);
      const mx = Math.max(qx, 0), my = Math.max(qy, 0);
      const d = Math.hypot(mx, my) - rr; // 边界处 0，玻璃内为负
      if (d > 0) continue; // 玻璃外：中性
      const t = Math.max(0, Math.min(1, 1 + d / band));
      if (t === 0) continue; // 深处：中性
      const e = t * t * (3 - 2 * t); // smoothstep
      let nx = 0, ny = 0;
      if (mx > 0 || my > 0) {
        const L = Math.hypot(mx, my) || 1;
        nx = mx / L; ny = my / L;
      } else if (qx > qy) nx = 1;
      else ny = 1;
      nx *= Math.sign(px - hw) || 1;
      ny *= Math.sign(py - hh) || 1;
      data[i] = 128 + 127 * nx * e;
      data[i + 1] = 128 + 127 * ny * e;
    }
  }
  return { data, width: W, height: H };
}
