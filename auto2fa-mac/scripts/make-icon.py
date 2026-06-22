#!/usr/bin/env python3
"""Generate Auto2FA macOS app icon set.

Renders a 1024x1024 master PNG (dark navy gradient with a three-dot
triangle-path symbol echoing SF Symbol `point.3.connected.trianglepath`)
then sips it down to every size macOS needs in an AppIcon.appiconset.
"""
import math
import os
import subprocess
from PIL import Image, ImageDraw, ImageFilter

SIZE = 1024
RADIUS = 224  # macOS-style corner radius for 1024px


def background():
    """Dark vertical gradient navy → near-black."""
    img = Image.new("RGB", (SIZE, SIZE), (0, 0, 0))
    top = (28, 41, 80)        # rich navy
    bottom = (8, 12, 28)      # almost black
    for y in range(SIZE):
        t = y / (SIZE - 1)
        r = int(top[0] * (1 - t) + bottom[0] * t)
        g = int(top[1] * (1 - t) + bottom[1] * t)
        b = int(top[2] * (1 - t) + bottom[2] * t)
        for x in range(SIZE):
            img.putpixel((x, y), (r, g, b))
    return img


def background_fast():
    """Same gradient via vectorised numpy-free approach using Image.new + paste."""
    img = Image.new("RGB", (SIZE, SIZE), (0, 0, 0))
    top = (28, 41, 80)
    bottom = (8, 12, 28)
    grad = Image.new("RGB", (1, SIZE), (0, 0, 0))
    pixels = grad.load()
    for y in range(SIZE):
        t = y / (SIZE - 1)
        pixels[0, y] = (
            int(top[0] * (1 - t) + bottom[0] * t),
            int(top[1] * (1 - t) + bottom[1] * t),
            int(top[2] * (1 - t) + bottom[2] * t),
        )
    return grad.resize((SIZE, SIZE))


def rounded_mask():
    mask = Image.new("L", (SIZE, SIZE), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle((0, 0, SIZE - 1, SIZE - 1), radius=RADIUS, fill=255)
    return mask


def draw_symbol(canvas: Image.Image):
    """Three glowing dots in a triangle, connected by lines."""
    d = ImageDraw.Draw(canvas, "RGBA")
    cx, cy = SIZE / 2, SIZE / 2 + 30
    radius_layout = 260      # distance from centre to each dot
    dot_radius = 100
    accent = (96, 220, 255)  # soft cyan
    glow = (96, 220, 255, 70)

    # Equilateral triangle with one vertex pointing up
    pts = []
    for i in range(3):
        a = math.radians(-90 + i * 120)
        pts.append((cx + radius_layout * math.cos(a),
                    cy + radius_layout * math.sin(a)))

    # Connecting lines first (so dots sit on top)
    for i in range(3):
        x1, y1 = pts[i]
        x2, y2 = pts[(i + 1) % 3]
        d.line([(x1, y1), (x2, y2)], fill=(96, 220, 255, 110), width=14)

    # Glow halo
    glow_layer = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow_layer)
    for (x, y) in pts:
        gd.ellipse((x - dot_radius * 1.6, y - dot_radius * 1.6,
                    x + dot_radius * 1.6, y + dot_radius * 1.6), fill=glow)
    glow_layer = glow_layer.filter(ImageFilter.GaussianBlur(radius=24))
    canvas.alpha_composite(glow_layer)

    # Solid dots
    d2 = ImageDraw.Draw(canvas, "RGBA")
    for (x, y) in pts:
        d2.ellipse((x - dot_radius, y - dot_radius,
                    x + dot_radius, y + dot_radius), fill=accent + (255,))
        # inner highlight
        d2.ellipse((x - dot_radius * 0.4, y - dot_radius * 0.7,
                    x + dot_radius * 0.4, y - dot_radius * 0.1),
                   fill=(255, 255, 255, 70))

    # "2FA" wordmark below the triangle
    try:
        from PIL import ImageFont
        # macOS system font path
        for candidate in (
            "/System/Library/Fonts/SF-Pro-Display-Bold.otf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial Bold.ttf",
        ):
            if os.path.exists(candidate):
                font = ImageFont.truetype(candidate, 130)
                break
        else:
            font = ImageFont.load_default()
        text = "2FA"
        bbox = d2.textbbox((0, 0), text, font=font)
        tw = bbox[2] - bbox[0]
        th = bbox[3] - bbox[1]
        tx = (SIZE - tw) / 2 - bbox[0]
        ty = cy + radius_layout + 80 - bbox[1]
        # subtle shadow
        d2.text((tx + 3, ty + 3), text, fill=(0, 0, 0, 130), font=font)
        d2.text((tx, ty), text, fill=(225, 235, 250, 235), font=font)
    except Exception as e:
        print(f"font draw failed: {e}")


def make_master():
    bg = background_fast().convert("RGBA")
    # Apply rounded corners
    rounded = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    rounded.paste(bg, mask=rounded_mask())
    draw_symbol(rounded)
    return rounded


def write_iconset(master: Image.Image, out_dir: str):
    sizes = [
        (16, 1), (16, 2),
        (32, 1), (32, 2),
        (128, 1), (128, 2),
        (256, 1), (256, 2),
        (512, 1), (512, 2),
    ]
    os.makedirs(out_dir, exist_ok=True)
    for s, scale in sizes:
        px = s * scale
        resized = master.resize((px, px), Image.LANCZOS)
        suffix = f"@{scale}x" if scale != 1 else ""
        fname = f"icon_{s}x{s}{suffix}.png"
        resized.save(os.path.join(out_dir, fname))
        print(f"  {fname}: {px}x{px}")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    iconset_dir = os.path.join(here, "..", "Auto2FA", "Assets.xcassets",
                               "AppIcon.appiconset")
    iconset_dir = os.path.abspath(iconset_dir)
    print(f"generating master 1024×1024…")
    master = make_master()
    # also save a master preview
    master.save(os.path.join(here, "..", "icon-preview.png"))
    print(f"writing iconset to {iconset_dir}…")
    write_iconset(master, iconset_dir)
    print("done.")


if __name__ == "__main__":
    main()
