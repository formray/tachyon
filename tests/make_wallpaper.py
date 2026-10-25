#!/usr/bin/env python3
"""Procedural 'busy' wallpaper for the screenshots (no third-party imagery):
soft colour fields, diagonal bands and fine detail, so any rectangle or halo around a
layered window would show. usage: make_wallpaper.py out.jpg [width height]"""
import sys
import numpy as np
from PIL import Image, ImageFilter

out = sys.argv[1] if len(sys.argv) > 1 else "wall_demo.jpg"
w, h = (int(sys.argv[2]), int(sys.argv[3])) if len(sys.argv) > 3 else (1600, 1000)
rng = np.random.default_rng(7)
y, x = np.mgrid[0:h, 0:w].astype(np.float32)
img = np.zeros((h, w, 3), np.float32)
img[:] = (18, 20, 46)
palette = [(255, 120, 60), (90, 70, 255), (40, 210, 255), (255, 70, 150), (255, 200, 80), (60, 255, 170)]
for i in range(34):
    cx, cy = rng.uniform(0, w), rng.uniform(0, h)
    r = rng.uniform(0.04, 0.14) * w
    c = np.array(palette[i % len(palette)], np.float32)
    a = np.exp(-(((x - cx) ** 2 + (y - cy) ** 2) / (2 * r * r))) * rng.uniform(0.55, 0.95)
    img = img * (1 - a[..., None]) + c * a[..., None]
bands = 0.5 + 0.5 * np.sin((x * 0.8 + y * 0.6) / 37.0 + np.sin(y / 90.0) * 2.0)
img *= (0.55 + 0.6 * bands)[..., None]
# thin bright contour lines (fine detail)
lines = np.abs(np.sin((x * 0.35 - y * 0.9) / 11.0 + np.cos(x / 140.0) * 3.0)) < 0.06
img[lines] = img[lines] * 0.6 + 255 * 0.4
img += rng.normal(0, 7, (h, w, 1)).astype(np.float32)
im = Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(0.6))
im.save(out, quality=92)
print(out, im.size)
