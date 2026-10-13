"""Compose the NERUDO share artwork from the approved vector assets and local font."""
from pathlib import Path
import base64
import re
import json

root = Path(__file__).resolve().parents[1]
brand = root / "public" / "brand"
display = base64.b64encode((root / "public" / "fonts" / "nerudo-display.woff2").read_bytes()).decode()


def contents(name):
    text = (brand / name).read_text()
    return re.sub(r'^<svg[^>]*>|</svg>\s*$', '', text.strip())


art = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630" role="img" aria-label="NERUDO. Good work takes form. A Git forge for people and coding agents.">
<style>@font-face{{font-family:NerudoDisplay;src:url(data:font/woff2;base64,{display}) format('woff2');font-weight:100 900}} .headline{{font-family:NerudoDisplay,Georgia,serif;font-weight:450;font-size:92px;letter-spacing:-4px;fill:#25302B}} .form-line{{fill:#284A3B}}</style>
<rect width="1200" height="630" fill="#F5F0E6"/>
<g transform="translate(70 54) scale(.74)">{contents('nerudo-wordmark.svg')}</g>
<path d="M939 76C1087 54 1170 153 1180 282C1193 423 1110 555 970 555C829 555 724 482 713 357C704 238 784 100 939 76Z" fill="#284A3B"/>
<path d="M941 104C1064 89 1140 174 1153 288C1165 416 1093 525 974 526C851 527 752 464 742 354C733 250 807 129 941 104Z" fill="none" stroke="#E9E2D5" stroke-opacity=".12"/>
<g transform="translate(755 119) scale(6.65)" fill="none">{contents('nerudo-strand-material.svg')}</g>
<text class="headline" x="70" y="272">Good work</text>
<text class="headline form-line" x="70" y="369">takes form<tspan fill="#985436">.</tspan></text>
<text x="74" y="459" font-family="Georgia,serif" font-size="23" fill="#646A60">A Git forge for people and coding agents.</text>
<path d="M74 525H649" stroke="#C9C9BC"/>
<text x="74" y="558" font-family="monospace" font-size="12" fill="#646A60" letter-spacing="1">PREPARE / REFINE / BRING TOGETHER</text>
</svg>'''
(brand / "nerudo-social.svg").write_text(art+"\n")
tokens = brand / "nerudo-tokens.json"
data = json.loads(tokens.read_text())
data["shareImage"] = {"vector":"nerudo-social.svg", "raster":"nerudo-social.png", "width":1200, "height":630, "alt":"NERUDO — Good work takes form. A Git forge for people and coding agents."}
for name in ("nerudo-social.svg", "nerudo-social.png"):
    if name not in data["files"]:
        data["files"].append(name)
tokens.write_text(json.dumps(data,indent=2)+"\n")
print("Built self-contained 1200 × 630 share artwork.")
