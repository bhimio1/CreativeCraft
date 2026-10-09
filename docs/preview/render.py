# Renders a static preview of the CreativeCraft Apps screen using the same sizes and colours as
# crates/ui-egui (theme.rs / lib.rs). Approximation of egui's rendering, for presentation only.
import sys
from PIL import Image, ImageDraw, ImageFont
A = sys.argv[1]; OUT = sys.argv[2]
S = 2
W, H = 1080, 720
def c(h): return tuple(int(h[i:i+2], 16) for i in (1, 3, 5))
bg, bar, row_hover, line = c('#1b1b1b'), c('#141414'), c('#2b2b2b'), c('#303030')
ink, muted, cream, on_cream, ok = c('#f0efe9'), c('#98978f'), c('#f0efe9'), c('#161616'), c('#7fc48c')
F = '/usr/share/fonts/opentype/inter/'
def font(w, size): return ImageFont.truetype(F + ('Inter-SemiBold.otf' if w else 'Inter-Regular.otf'), int(size * S))
img = Image.new('RGB', (W*S, H*S), bg); d = ImageDraw.Draw(img)
def R(x, y, w, h): return [x*S, y*S, (x+w)*S, (y+h)*S]
def text(x, y, s, size, col, bold=False, anchor='la'): d.text((x*S, y*S), s, font=font(bold, size), fill=col, anchor=anchor)
def paste(path, x, y, w, h):
    im = Image.open(path).convert('RGBA').resize((int(w*S), int(h*S)), Image.LANCZOS); img.paste(im, (int(x*S), int(y*S)), im)
def pill(x, y, w, h, fill=None, outline=None, label='', col=ink, bold=False):
    d.rounded_rectangle(R(x, y, w, h), radius=14*S, fill=fill, outline=outline, width=S)
    text(x + w/2, y + h/2, label, 13, col, bold, 'mm')

# top bar
d.rectangle(R(0, 0, W, 52), fill=bar)
logo = Image.open(f'{A}/brand/artcraft-logo-white.png'); lw = 15 * logo.width / logo.height
paste(f'{A}/brand/artcraft-logo-white.png', 18, 18.5, lw, 15)
d.rectangle(R(18 + lw + 10, 18, 1, 16), fill=line)
text(18 + lw + 21, 26, 'CreativeCraft', 14, muted, anchor='lm')
d.rounded_rectangle(R(380, 12, 320, 28), radius=6*S, fill=c('#0d0d0d'), outline=line, width=S)
text(392, 26, 'Search apps', 13, muted, anchor='lm')
d.text(((W-30)*S, 26*S), '⚙', font=ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf', 18*S), fill=ink, anchor='mm')
d.ellipse(R(W - 66, 22, 8, 8), fill=ok); text(W - 74, 26, 'Cloud on', 12, muted, anchor='rm')

# sidebar
d.rectangle(R(0, 52, 200, H - 52), fill=bar)
for i, (lbl, sel) in enumerate([('Apps', True), ('Files', False), ('Assistant', False)]):
    y = 66 + i * 42
    if sel: d.rounded_rectangle(R(10, y, 180, 34), radius=6*S, fill=row_hover)
    text(22, y + 17, lbl, 14, ink if sel else muted, anchor='lm')

import os
MODE = os.environ.get('MODE', 'apps')
if MODE == 'assistant':
    d.rectangle(R(10, 66, 180, 34), fill=bar)
    text(22, 83, 'Apps', 14, muted, anchor='lm')
    d.rounded_rectangle(R(10, 66 + 84, 180, 34), radius=6*S, fill=row_hover)
    text(22, 83 + 84, 'Assistant', 14, ink, anchor='lm')
    x0 = 232
    text(x0, 76, 'Assistant', 18, ink, True)
    text(x0 + 110, 79, 'Claude Code', 13, muted)
    d.text(((x0 + 197)*S, 80*S), '▾', font=ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf', 12*S), fill=muted)
    d.ellipse(R(x0 + 222, 82, 8, 8), fill=ok)
    text(x0, 150, 'What would you like to make?', 22, ink, True)
    text(x0, 186, 'The assistant can install, open and work inside your apps.', 14, muted)
    for i, sug in enumerate(['Which of my apps can you control?', 'Open PhotoCraft and start a 1080×1080 canvas with a warm gradient background', 'Install VectorCraft and trace logo.png into a vector', 'Sync my projects, then tell me what changed']):
        y = 222 + i * 40
        tw = d.textlength(sug, font=font(False, 13)) / S
        d.rounded_rectangle(R(x0, y, tw + 28, 30), radius=6*S, fill=c('#232323'))
        text(x0 + 14, y + 15, sug, 13, ink, anchor='lm')
    d.rounded_rectangle(R(x0, H - 24 - 80, W - x0 - 32 - 90, 80), radius=6*S, fill=c('#0d0d0d'), outline=line, width=S)
    text(x0 + 10, H - 24 - 70, 'Ask CreativeCraft to… (Enter to send, Shift+Enter for a new line)', 13, muted)
    pill(W - 32 - 80, H - 24 - 80, 76, 28, fill=cream, label='Send', col=on_cream, bold=True)
    img.save(OUT); sys.exit()
# content
x0, y = 232, 76
cw = W - x0 - 32
def section(title, rows, y):
    text(x0, y, title, 18, ink, True); y += 34
    for (icon, name, sub, action, hover) in rows:
        if hover: d.rounded_rectangle(R(x0, y, cw, 64), radius=8*S, fill=row_hover)
        if icon: paste(icon, x0 + 12, y + 10, 44, 44)
        else:
            d.rounded_rectangle(R(x0 + 12, y + 10, 44, 44), radius=10*S, fill=line)
            text(x0 + 34, y + 32, name[0], 20, muted, anchor='mm')
        text(x0 + 70, y + 14, name, 15, ink, True)
        text(x0 + 70, y + 36, sub, 12, muted)
        right = x0 + cw - 12
        if action == 'open':
            pill(right - 76, y + 18, 76, 28, fill=cream, label='Open', col=on_cream, bold=True)
            text(right - 100, y + 30, '…', 16, ink, anchor='mm')
        elif action == 'openx':
            pill(right - 76, y + 18, 76, 28, fill=cream, label='Open', col=on_cream, bold=True)
        elif action == 'progress':
            d.rounded_rectangle(R(right - 230, y + 29, 140, 6), radius=3*S, fill=line)
            d.rounded_rectangle(R(right - 230, y + 29, 140 * 0.62, 6), radius=3*S, fill=cream)
            text(right, y + 32, '74 of 119 MB', 12, muted, anchor='rm')
        else:
            pill(right - 76, y + 18, 76, 28, outline=muted, label=action)
        y += 68
    return y + 22
ap = lambda n: f'{A}/apps/{n}.png'
y = section('Installed', [
    (ap('photocraft'), 'PhotoCraft', 'Image · 0.5.0', 'open', True),
    (ap('vectorcraft'), 'VectorCraft', 'Vector · 0.4.2', 'open', False),
    (None, 'Blender', '3D', 'openx', False),
], y)
y = section('Available', [
    (ap('filmcraft'), 'FilmCraft', 'Video', 'progress', False),
    (ap('lightcraft'), 'LightCraft', 'Photo', 'Install', False),
    (ap('printcraft'), 'PdfCraft', 'Documents', 'Install', False),
    (ap('effectcraft'), 'EffectCraft', 'Motion', 'Install', False),
], y)
img.save(OUT)
