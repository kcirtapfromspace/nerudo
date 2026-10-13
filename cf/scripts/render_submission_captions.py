#!/usr/bin/env python3
"""Replace the retained video's recording-overlay band, leaving product content above it.

Requires existing ffmpeg/ffprobe and Pillow. Does not open a browser or generate speech.
The old border is measured on every frame; missing borders produce transparent overlays.
An isolated compression-damaged frame between identical borders uses its neighbors.
For an already-cut video, --fixed-band-height paints the complete fixed footer band.
"""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def probe_video(video):
    data = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", str(video)
    ], text=True))
    stream = next(s for s in data["streams"] if s["codec_type"] == "video")
    a, b = stream["avg_frame_rate"].split("/")
    fps = float(a) / float(b)
    duration = float(stream.get("duration") or data["format"]["duration"])
    if fps <= 0 or duration <= 0:
        raise ValueError("Video frame rate and measured duration must be positive")
    return stream["width"], stream["height"], fps, duration


def read_exact(stream, count):
    result = bytearray()
    while len(result) < count:
        chunk = stream.read(count - len(result))
        if not chunk:
            break
        result.extend(chunk)
    return result


def detect_bands(video, width, height, fps):
    window = 200
    filters = (f"[0:v]split[l][r];[l]crop=20:{window}:0:ih-{window}[a];"
               f"[r]crop=20:{window}:iw-20:ih-{window}[b];[a][b]hstack")
    process = subprocess.Popen([
        "ffmpeg", "-v", "error", "-i", str(video), "-filter_complex", filters,
        "-pix_fmt", "rgb24", "-f", "rawvideo", "-"
    ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    groups = []
    frame_count = detected = 0
    target = (201, 196, 185)  # The original recording overlay's opaque top border.
    try:
        while True:
            frame = read_exact(process.stdout, 40 * window * 3)
            if not frame:
                break
            if len(frame) != 40 * window * 3:
                raise ValueError("Incomplete border-analysis frame")
            best = (float("inf"), None)
            for y in range(10, 86):  # Existing band is at least115px and at most190px.
                error = 0
                for x in (2, 5, 15, 22, 30, 37):
                    i = (y * 40 + x) * 3
                    error += sum(abs(frame[i + c] - target[c]) for c in range(3))
                candidate = (error / 18, y)
                if candidate[0] < best[0]:
                    best = candidate
            band = window - best[1] if best[0] < 12 else 0
            if band:
                detected += 1
            start = frame_count / fps
            if groups and groups[-1]["height"] == band:
                groups[-1]["end"] = (frame_count + 1) / fps
            else:
                groups.append({"start": start, "end": (frame_count + 1) / fps, "height": band})
            frame_count += 1
    finally:
        process.stdout.close()
    error = process.stderr.read().decode()
    if process.wait() != 0:
        raise RuntimeError(error)
    if not frame_count or detected / frame_count < 0.8:
        raise ValueError("Cannot identify the existing recording-overlay band reliably; refusing to cover product footage")
    interpolated = 0
    for i, group in enumerate(groups[1:-1], 1):
        previous, following = groups[i - 1], groups[i + 1]
        if (group["height"] == 0 and group["end"] - group["start"] <= 1 / fps + 0.000001
                and previous["height"] and previous["height"] == following["height"]):
            group["height"] = previous["height"]
            interpolated += 1
    groups[-1]["end"] = frame_count / fps
    return groups, {"frames": frame_count, "detected_frames": detected,
                    "detected_fraction": detected / frame_count,
                    "isolated_border_frames_interpolated": interpolated,
                    "band_heights": sorted({g["height"] for g in groups})}


def font_path(explicit=None, bold=False):
    if explicit:
        return explicit
    names = (["/System/Library/Fonts/Supplemental/Arial Bold.ttf",
              "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"] if bold else
             ["/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Helvetica.ttc",
              "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"])
    for name in names:
        if Path(name).is_file():
            return name
    raise ValueError("A readable TrueType font is required; pass --font/--bold-font")


def wrap(text, font, width):
    lines = []
    current = ""
    for word in text.split():
        candidate = f"{current} {word}".strip()
        if current and font.getlength(candidate) > width:
            lines.append(current)
            current = word
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines


def validate_fixed_timeline(mapping, layers, duration, fps):
    """A fixed footer follows the cut video's identity timeline, with no reused captions."""
    tolerance = 1 / fps + 0.000001
    previous = 0.0
    for chapter in mapping:
        if (abs(chapter["source_start"] - previous) > tolerance
                or abs(chapter["source_start"] - chapter["render_start"]) > 0.000001
                or abs(chapter["source_end"] - chapter["render_end"]) > 0.000001
                or abs(chapter["scale"] - 1) > 0.000001
                or chapter["source_end"] <= chapter["source_start"]):
            raise ValueError("Fixed-band mode requires a contiguous identity chapter map for the already-cut video")
        previous = chapter["source_end"]
    if abs(previous - duration) > tolerance:
        raise ValueError("Identity chapter map must match the measured cut-video duration within one frame")
    previous = 0.0
    for layer in layers:
        if (layer["end"] <= layer["start"]
                or abs(layer["start"] - previous) > 0.000001):
            raise ValueError("Fixed-band caption layers must be contiguous and must not overlap")
        previous = layer["end"]
    if abs(previous - duration) > tolerance:
        raise ValueError("Caption layers must cover the measured cut-video duration within one frame")


def image_for(layer, band, width, output, regular, bold):
    payload = json.dumps({"layer": layer, "height": band, "width": width,
                          "regular_font": regular, "bold_font": bold}, sort_keys=True)
    filename = hashlib.sha256(payload.encode()).hexdigest()[:20] + ".png"
    file = output / filename
    if file.exists():
        return file
    image = Image.new("RGBA", (width, 200), (0, 0, 0, 0))
    if band:
        draw = ImageDraw.Draw(image)
        top = 200 - band
        draw.rectangle((0, top, width, 200), fill=(247, 246, 242, 255))
        draw.line((0, top, width, top), fill=(201, 196, 185, 255))
        title_font = ImageFont.truetype(bold, 16)
        label_font = ImageFont.truetype(regular, 12)
        caption_font = ImageFont.truetype(regular, 18)
        label = f"{layer['label']} · {layer['url']}"
        if title_font.getlength(layer["title"]) > width - 52 or label_font.getlength(label) > width - 52:
            raise ValueError("Chapter label/URL exceeds the existing band width; shorten the label")
        lines = wrap(layer.get("caption", ""), caption_font, width - 52)
        if len(lines) > 2:
            raise ValueError("Caption needs more than two lines; split its timing into shorter cues")
        y = 200 - 115 + 10  # Anchor text consistently inside the minimum existing band.
        draw.text((26, y), layer["title"], fill=(40, 39, 34, 255), font=title_font)
        draw.text((26, y + 23), label, fill=(103, 99, 91, 255), font=label_font)
        for i, line in enumerate(lines):
            draw.text((26, y + 45 + i * 23), line, fill=(40, 39, 34, 255), font=caption_font)
    image.save(file)
    return file


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--video", required=True)
    parser.add_argument("--layers", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--chapter-map", help="Source-to-render map; optional for fixed-band identity mode")
    parser.add_argument("--fixed-band-height", type=int,
                        help="Paint this complete bottom band on an already-cut video (115–200 pixels)")
    parser.add_argument("--font")
    parser.add_argument("--bold-font")
    args = parser.parse_args()
    video, output = Path(args.video), Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    layers = json.loads(Path(args.layers).read_text())
    width, height, fps, source_duration = probe_video(video)
    if not layers:
        raise ValueError("At least one caption layer is required")
    if args.fixed_band_height is not None:
        if not 115 <= args.fixed_band_height <= min(200, height):
            raise ValueError("Fixed band must be115–200 pixels high and fit the video")
        mapping = (json.loads(Path(args.chapter_map).read_text()) if args.chapter_map else
                   [{"source_start": 0, "source_end": source_duration, "render_start": 0,
                     "render_end": source_duration, "scale": 1}])
        if not mapping:
            raise ValueError("Chapter map cannot be empty")
        validate_fixed_timeline(mapping, layers, source_duration, fps)
        total = source_duration
        bands = [{"start": 0, "end": total, "height": args.fixed_band_height}]
        measured = {"mode": "explicit_fixed_band", "band_heights": [args.fixed_band_height],
                    "source_duration_seconds": source_duration,
                    "product_height_preserved": height - args.fixed_band_height,
                    "border_detection_bypassed": True}
        scope = (f"Complete bottom {args.fixed_band_height}px opaque; product pixels above "
                 f"y={height - args.fixed_band_height} remain transparent in the overlay")
    else:
        if not args.chapter_map:
            parser.error("--chapter-map is required unless --fixed-band-height is supplied")
        source_bands, measured = detect_bands(video, width, height, fps)
        mapping = json.loads(Path(args.chapter_map).read_text())
        if not mapping:
            raise ValueError("Chapter map cannot be empty")
        total = mapping[-1]["render_end"]
        bands = []
        for band in source_bands:
            for chapter in mapping:
                start = max(band["start"], chapter["source_start"])
                end = min(band["end"], chapter["source_end"])
                if end <= start:
                    continue
                bands.append({"start": chapter["render_start"] + (start - chapter["source_start"]) * chapter["scale"],
                              "end": chapter["render_start"] + (end - chapter["source_start"]) * chapter["scale"],
                              "height": band["height"]})
        measured["source_duration_seconds"] = source_duration
        scope = "Only detected existing recording-overlay pixels; transparent above each measured border"
    if not bands:
        raise ValueError("No overlay band intersects the requested render timeline")
    regular, bold = font_path(args.font), font_path(args.bold_font, True)
    boundaries = sorted(t for t in {0.0, total, *(s["start"] for s in layers), *(s["end"] for s in layers),
                                   *(s["start"] for s in bands), *(s["end"] for s in bands)}
                        if 0 <= t <= total)
    commands = []
    band_index = layer_index = 0
    last = None
    for start, end in zip(boundaries, boundaries[1:]):
        if end <= start:
            continue
        midpoint = (start + end) / 2
        while band_index + 1 < len(bands) and bands[band_index]["end"] <= midpoint:
            band_index += 1
        while layer_index + 1 < len(layers) and layers[layer_index]["end"] <= midpoint:
            layer_index += 1
        file = image_for(layers[layer_index], bands[band_index]["height"], width, output, regular, bold)
        quoted = str(file).replace("'", "'\\''")
        commands.extend([f"file '{quoted}'", f"duration {end - start:.9f}"])
        last = quoted
    commands.append(f"file '{last}'")
    (output / "concat.txt").write_text("\n".join(commands) + "\n")
    metadata = {"width": width, "height": height, "fps": fps, "overlay_window_height": 200,
                "replacement_scope": scope, "fixed_band_height": args.fixed_band_height,
                "source_duration_seconds": source_duration,
                "measurement": measured, "render_duration_seconds": total, "chapter_retiming": mapping, "layers": len(layers), "segments": (len(commands) - 1) // 2}
    (output / "overlay-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata))


if __name__ == "__main__":
    main()
