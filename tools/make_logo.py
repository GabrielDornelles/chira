#!/usr/bin/env python3
"""Build the Chira logo SVGs (text converted to outlines, so they look the same everywhere).

    pip install fonttools
    # Nunito variable font: https://github.com/google/fonts/tree/main/ofl/nunito
    python3 tools/make_logo.py path/to/Nunito[wght].ttf

Writes docs/img/logo-dark.svg, docs/img/logo-light.svg and docs/img/favicon.svg.
"""
import sys
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

font = instancer.instantiateVariableFont(TTFont(sys.argv[1]), {"wght": 200})     # ExtraLight
gs, cmap = font.getGlyphSet(), font.getBestCmap()
TEXT, TRACK = "chira", 220                                                         # tracking .22em, in font units (1000/em)

x, paths, top, a_end = 0, [], 0, 0
for i, ch in enumerate(TEXT):
    g = gs[cmap[ord(ch)]]
    bp = BoundsPen(gs); g.draw(bp)
    top = max(top, bp.bounds[3])
    pen = SVGPathPen(gs, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
    g.draw(TransformPen(pen, (1, 0, 0, -1, x, 0)))                                 # y flipped, baseline at y = 0
    paths.append(pen.getCommands())
    if i == len(TEXT) - 1:
        a_end = x + bp.bounds[2]
    x += g.width + TRACK

STAR = "M12 0C13 8 16 11 24 12C16 13 13 16 12 24C11 16 8 13 0 12C8 11 11 8 12 0Z"
def star(cx, cy, size, cls, opacity=1):
    k = size / 24
    return f'<g opacity="{opacity}" transform="translate({cx - size / 2:.1f} {cy - size / 2:.1f}) scale({k:.4f})"><path class="s {cls}" d="{STAR}"/></g>'

ytop = -top
glints = (star(a_end + 420, ytop - 210, 200, "s1") + star(a_end + 700, ytop + 70, 110, "s2", .7) + star(a_end + 250, ytop - 450, 90, "s3", .55))
x0, x1, y0, y1 = -20, a_end + 790, ytop - 520, 40
W, H = x1 - x0, y1 - y0

CSS = (".s{transform-box:fill-box;transform-origin:center;animation:tw 3.2s ease-in-out infinite}.s2{animation-delay:.8s}.s3{animation-delay:1.6s}"
       "@keyframes tw{0%,100%{transform:scale(.7) rotate(0);opacity:.35}50%{transform:scale(1) rotate(25deg);opacity:1}}"
       "@media (prefers-reduced-motion:reduce){.s{animation:none}}")

def logo(ink, glint, name):
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x0} {y0:.0f} {W:.0f} {H:.0f}" width="{W / 5:.0f}" height="{H / 5:.0f}" role="img" aria-label="chira">'
           f'<style>{CSS}.g{{fill:{glint}}}</style><g fill="{ink}">' + "".join(f'<path d="{p}"/>' for p in paths) + f'</g><g class="g">{glints}</g></svg>')
    svg = svg.replace('class="s ', 'class="g s ')
    open(f"docs/img/{name}", "w").write(svg)
    print(name, len(svg), "bytes")

logo("#e6e0f5", "#bda7ff", "logo-dark.svg")
logo("#3a3150", "#8f6cf5", "logo-light.svg")
open("docs/img/favicon.svg", "w").write(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="16" fill="#0d0c12"/>'
    f'<path transform="translate(14 14) scale(1.5)" fill="#bda7ff" d="{STAR}"/>'
    f'<path transform="translate(44 12) scale(.45)" fill="#bda7ff" opacity=".6" d="{STAR}"/></svg>')
print("favicon.svg")
