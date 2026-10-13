"""Draw the N as parallel handmade noodles; share geometry across UI and SVG assets."""
from pathlib import Path
import json
import math
import re

root = Path(__file__).resolve().parents[1]
brand = root / "public" / "brand"
forms = [
    [(15, 51), (2, 44), (4, 28), (18, 26), (32, 24), (32, 7), (18, 13), (4, 19), (25, 58), (42, 51), (59, 44), (37, 16), (54, 14), (55, 14)],
    [(14, 51), (7, 41), (8, 28), (15, 19), (22, 10), (25, 11), (29, 21), (36, 38), (31, 44), (40, 49), (49, 54), (54, 48), (52, 38), (55, 14)],
    [(14, 51), (11, 42), (12, 29), (15, 19), (17, 11), (22, 11), (26, 19), (33, 32), (35, 43), (40, 49), (45, 56), (51, 52), (52, 43), (55, 14)],
]


def noodle(form, offset, index, irregular=True):
    points = []
    for segment in range(5):
        samples = 12 if segment < 4 else 6
        for sample in range(samples + 1):
            if segment and sample == 0:
                continue
            t = sample / samples
            if segment < 4:
                p0, p1, p2, p3 = form[segment * 3:segment * 3 + 4]
                point = tuple((1-t)**3*p0[a] + 3*(1-t)**2*t*p1[a] + 3*(1-t)*t*t*p2[a] + t**3*p3[a] for a in range(2))
                tangent = tuple(3*(1-t)**2*(p1[a]-p0[a]) + 6*(1-t)*t*(p2[a]-p1[a]) + 3*t*t*(p3[a]-p2[a]) for a in range(2))
            else:
                p0, p1 = form[-2:]
                point = tuple(p0[a] + (p1[a]-p0[a])*t for a in range(2))
                tangent = tuple(p1[a]-p0[a] for a in range(2))
            length = math.hypot(*tangent)
            tx, ty = tangent[0]/length, tangent[1]/length
            progress = (segment + t) / 5
            phase = index * 1.67
            wobble = (.09*math.sin(progress*math.pi*7+phase) + .035*math.sin(progress*math.pi*17+phase)) if irregular else 0
            # Cut ends differ slightly in length, like the hand-gathered reference.
            tail = (math.sin(phase+.7)*.9*max(0, 1-progress/.045)**2 + math.cos(phase)*.85*max(0, 1-(1-progress)/.045)**2) if irregular else 0
            points.append((point[0]-ty*(offset+wobble)+tx*tail, point[1]+tx*(offset+wobble)+ty*tail))
    fmt = lambda p: f"{p[0]:.3f} {p[1]:.3f}"
    result = "M" + fmt(points[0])
    for i in range(len(points)-1):
        a, b, c, d = points[max(0, i-1)], points[i], points[i+1], points[min(len(points)-1, i+2)]
        c1 = (b[0]+(c[0]-a[0])/6, b[1]+(c[1]-a[1])/6)
        c2 = (c[0]-(d[0]-b[0])/6, c[1]-(d[1]-b[1])/6)
        result += "C" + fmt(c1) + " " + fmt(c2) + " " + fmt(c)
    return result


geometry = {
    "stages": [[noodle(form, (i-5)*1.08, i) for i in range(11)] for form in forms],
    "mark": {"paths": [noodle(forms[2], (i-2)*2.35, i, False) for i in range(5)], "strokeWidth": 1.25},
    "compact": {"paths": [noodle(forms[2], (i-1)*3.8, i, False) for i in range(3)], "strokeWidth": 2.25},
    "materialWidth": .86,
}


def curve_bounds(path):
    numbers = [float(n) for n in re.findall(r'-?\d+(?:\.\d+)?', path)]
    start = numbers[:2]
    values = [[start[0]], [start[1]]]
    for i in range(2, len(numbers), 6):
        coords = numbers[i:i+6]
        for axis in range(2):
            p0, p1, p2, p3 = start[axis], coords[axis], coords[axis+2], coords[axis+4]
            a, b, c = -p0+3*p1-3*p2+p3, 2*(p0-2*p1+p2), p1-p0
            roots = []
            if abs(a) < 1e-9:
                if abs(b) > 1e-9:
                    roots = [-c/b]
            elif b*b-4*a*c >= 0:
                disc = math.sqrt(b*b-4*a*c)
                roots = [(-b+disc)/(2*a), (-b-disc)/(2*a)]
            for t in [0, 1] + [t for t in roots if 0 < t < 1]:
                values[axis].append((1-t)**3*p0+3*(1-t)**2*t*p1+3*(1-t)*t*t*p2+t**3*p3)
        start = coords[4:6]
    return min(values[0]), min(values[1]), max(values[0]), max(values[1])


extents = [curve_bounds(path) for path in geometry["stages"][2]]
radius = geometry["materialWidth"] / 2
x0, y0 = min(b[0] for b in extents)-radius, min(b[1] for b in extents)-radius
x1, y1 = max(b[2] for b in extents)+radius, max(b[3] for b in extents)+radius
geometry["bounds"] = {"x":round(x0,4), "y":round(y0,4), "width":round(x1-x0,4), "height":round(y1-y0,4)}
(root / "src" / "strand-geometry.json").write_text(json.dumps(geometry, indent=2)+"\n")


def flat(paths, width, color):
    return ''.join(f'<path d="{path}" fill="none" stroke="{color}" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"/>' for path in paths)


flat_mark = flat(geometry["mark"]["paths"], geometry["mark"]["strokeWidth"], "#284A3B")
(brand / "nerudo-mark.svg").write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="NERUDO handmade noodle N">{flat_mark}</svg>\n')
compact = flat(geometry["compact"]["paths"], geometry["compact"]["strokeWidth"], "#284A3B")
(brand / "nerudo-icon.svg").write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="10" fill="#F5F0E6"/>{compact}</svg>\n')
for name, color in [("nerudo-wordmark.svg", "#284A3B"), ("nerudo-wordmark-reversed.svg", "#FFFDF8")]:
    target = brand / name
    text = target.read_text()
    mark = flat(geometry["mark"]["paths"], geometry["mark"]["strokeWidth"], color)
    text, count = re.subn(r'(<g transform="translate\(0 0\)">).*?</g>', lambda m: m[1]+mark+'</g>', text, count=1)
    assert count == 1, f"Missing mark group in {name}"
    target.write_text(text)

defs = '''<linearGradient id="dough" x1="12" y1="11" x2="53" y2="54" gradientUnits="userSpaceOnUse"><stop stop-color="#EEEBDC"/><stop offset=".4" stop-color="#D9D3BA"/><stop offset=".75" stop-color="#C5BCA0"/><stop offset="1" stop-color="#E3DFCE"/></linearGradient><filter id="flour" x="-5%" y="-5%" width="110%" height="110%" color-interpolation-filters="sRGB"><feTurbulence type="fractalNoise" baseFrequency="2.4" numOctaves="3" seed="17" result="grain"/><feColorMatrix in="grain" type="matrix" values="0 0 0 0 1 0 0 0 0 1 0 0 0 0 .95 .7 .7 .7 0 -.94" result="flour"/><feComposite in="flour" in2="SourceGraphic" operator="in" result="dust"/><feBlend in="SourceGraphic" in2="dust" mode="screen"/></filter>'''
material = ''.join(f'<g><path d="{path}" stroke="#A69B7D" stroke-opacity=".45" stroke-width="1.08" transform="translate(.08 .12)"/><path d="{path}" stroke="url(#dough)" stroke-width=".86"/><path d="{path}" stroke="#F4F1E4" stroke-opacity=".6" stroke-width=".22" transform="translate(-.13 -.08)"/></g>' for path in geometry["stages"][2])
(brand / "nerudo-strand-material.svg").write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none" role="img" aria-label="NERUDO N formed from fine flour-dusted noodles"><defs>{defs}</defs><g filter="url(#flour)" stroke-linecap="butt" stroke-linejoin="round">{material}</g></svg>\n')

tokens = brand / "nerudo-tokens.json"
data = json.loads(tokens.read_text())
data["mark"].pop("path", None)
data["mark"].update({"concept":"A bundle of fine handmade, flour-dusted noodles gathered into an N", "paths":geometry["mark"]["paths"], "strokeWidth":1.25, "clearSpace":"One bundle-width", "bounds":geometry["bounds"], "use":"Five strands for the flat mark; three for icons at 24 px and below. Eleven fine, matte flour-dusted strands for display. Keep the N contour and open gaps.", "formationStages":["Prepare: loose noodle folds", "Refine: strands drawn together", "Form: measured noodle bundle"]})
data["visualLanguage"]["illustration"] = "Fine, pale handmade noodles with matte flour texture, uneven cut ends, and slight natural variation. Eleven strands share a measured N contour."
tokens.write_text(json.dumps(data,indent=2)+"\n")
print("Built shared noodle geometry and five SVG assets.")
