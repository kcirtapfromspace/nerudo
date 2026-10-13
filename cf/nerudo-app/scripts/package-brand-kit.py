"""Package the public NERUDO kit from its current source assets and theme files."""
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile
import json
from xml.etree import ElementTree

root = Path(__file__).resolve().parents[1]
brand = root / "public" / "brand"
assets = sorted(brand.glob("*.svg")) + sorted(brand.glob("*.png")) + [brand / "nerudo-tokens.json"]
fonts = sorted((root / "public" / "fonts").glob("*.woff2"))
licenses = sorted((root / "public" / "fonts").glob("*-OFL.txt"))
styles = [root / "src" / name for name in ("nerudo.css", "brand-kit.css", "formation.css", "reading.css")]
geometry = json.loads((root / "src" / "strand-geometry.json").read_text())
for path in assets:
    if path.suffix == ".svg" and path.name != "nerudo-social.svg":
        shapes = [p for p in ElementTree.parse(path).iter("{http://www.w3.org/2000/svg}path") if "stroke" in p.attrib]
        variant = geometry["compact"] if path.name == "nerudo-icon.svg" else geometry["mark"]
        expected = geometry["stages"][2] if path.name == "nerudo-strand-material.svg" else variant["paths"]
        assert shapes and set(p.attrib["d"] for p in shapes) == set(expected), f"Stale noodle geometry: {path.name}"
        if path.name != "nerudo-strand-material.svg":
            assert all(float(p.attrib["stroke-width"]) == variant["strokeWidth"] for p in shapes), f"Stale noodle width: {path.name}"
with ZipFile(brand / "nerudo-brand-kit.zip", "w", ZIP_DEFLATED) as archive:
    for path in assets + fonts + licenses + styles:
        folder = "theme" if path in styles else "fonts" if path in fonts + licenses else "brand"
        archive.write(path, f"nerudo/{folder}/{path.name}")
    assert archive.testzip() is None
print(f"Packaged {len(assets + fonts + licenses + styles)} files.")
