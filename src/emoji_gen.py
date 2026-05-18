import subprocess, math, re, os, sys
from PIL import Image

src  = os.environ['MOFI_EMOJI_SRC']
out  = os.environ['MOFI_EMOJI_OUT']
meta = os.environ['MOFI_EMOJI_META']
COLS = int(os.environ['MOFI_EMOJI_COLS'])
CW   = int(os.environ['MOFI_EMOJI_CW'])
CH   = int(os.environ['MOFI_EMOJI_CH'])

raw = re.findall(r'\("([^"]+)"', open(src).read())
if not raw:
    sys.exit(1)

rows = math.ceil(len(raw) / COLS)
sheet = Image.new('RGBA', (COLS * CW, rows * CH), (0, 0, 0, 0))

for ri in range(rows):
    batch = raw[ri*COLS:(ri+1)*COLS]
    subprocess.run(
        ['pango-view', '--font=Noto Color Emoji 40',
         '--output=/tmp/_mofi_row.png', '--no-display',
         '--background=transparent',
         '--text=' + ''.join(batch)],
        capture_output=True
    )
    try:
        row = Image.open('/tmp/_mofi_row.png').convert('RGBA')
    except Exception:
        continue
    acw = max(1, row.width // len(batch))
    for ci in range(len(batch)):
        cell = row.crop((ci * acw, 0, (ci + 1) * acw, row.height))
        sheet.paste(cell, (
            ci * CW + (CW - cell.width) // 2,
            ri * CH + (CH - cell.height) // 2,
        ))

sheet.save(out)
open(meta, 'w').write(str(len(raw)))
print(f"Generated {len(raw)} emojis -> {out}")
