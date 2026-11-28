# Writes assets/src/example.html; render with scripts/render_assets.sh.
W,H=1800,440
FONT="-apple-system,Helvetica Neue,Arial,sans-serif"
Y,HH=90,170
boxes=[("FIX","Claude Code fixes add.py","a fresh session on its own git branch","var(--agent)",300),
       ("VERIFY","does python3 add.py print 3?","verifiers/tests.sh, run by Relentless","var(--verifier)",620),
       ("REVIEW","Codex grades the change","against verifiers/fix-review.md","var(--verifier)",940),
       ("APPROVE","you read add.py and answer","a checkpoint: the run waits for you","var(--human)",1260)]
html=f'''<!doctype html><meta charset="utf-8">
<style>
:root{{--agent:#2a9d8f;--verifier:#d63a46;--human:#f28c45;--engine:#264653;--ink:#1d2b33;--mute:#5b6b73}}
*{{box-sizing:border-box}}
html,body{{margin:0;background:#fff}}
body{{width:{W}px;height:{H}px;position:relative;font-family:{FONT};color:var(--ink)}}
.b{{position:absolute;top:{Y}px;height:{HH}px;border-radius:18px;display:flex;flex-direction:column;justify-content:center;align-items:center;text-align:center;padding:10px 16px;color:#fff;width:250px}}
.b h4{{margin:0;font-size:30px;letter-spacing:.06em;font-weight:800}}
.b p{{margin:8px 0 0;font-size:20px;line-height:1.3;font-weight:600}}
.b small{{display:block;margin-top:8px;font-size:16px;opacity:.92;line-height:1.3}}
.lbl{{position:absolute;font-size:15px;font-weight:800;letter-spacing:.12em;color:var(--mute);text-transform:uppercase}}
.cap{{position:absolute;text-align:center;font-weight:800}}
</style>
<div class="lbl" style="left:300px;top:36px">The example: fix add.py, prove it, let a human approve</div>
<img src="mark.png" style="position:absolute;left:30px;top:50px;width:200px;height:200px">
<div class="cap" style="left:20px;top:254px;width:220px;font-size:26px;letter-spacing:.04em;color:var(--engine)">Relentless</div>
<div class="cap" style="left:10px;top:290px;width:240px;font-size:16px;font-weight:600;color:var(--mute)">runs this workflow</div>
'''
for t,p,s,c,x in boxes:
    style=f"left:{x}px;background:{c}"
    if t=="APPROVE": style+=";color:#fff"
    html+=f'<div class="b" style="{style}"><h4>{t}</h4><p>{p}</p><small>{s}</small></div>\n'
html+=f'<div class="b" style="left:1580px;width:160px;background:var(--engine)"><h4 style="font-size:28px">✔ DONE</h4><p style="font-size:17px">add.py prints 3,<br>approved</p></div>\n'
html+=f'''<svg width="{W}" height="{H}" style="position:absolute;left:0;top:0" xmlns="http://www.w3.org/2000/svg">
<defs>
<marker id="a" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#264653"/></marker>
<marker id="r" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#d63a46"/></marker>
</defs>
<g stroke="#264653" stroke-width="3.5" fill="none" marker-end="url(#a)">
<path d="M240,175 H296"/><path d="M550,175 H616"/><path d="M870,175 H936"/><path d="M1190,175 H1256"/><path d="M1510,175 H1576"/>
</g>
<g font-family="{FONT}" font-size="18" font-weight="800" fill="#264653" text-anchor="middle">
<text x="583" y="160">met</text><text x="903" y="160">ok</text><text x="1223" y="160">yes</text>
</g>
<g stroke="#d63a46" stroke-width="3.5" fill="none" stroke-dasharray="9 7">
<path d="M745,260 V340"/><path d="M1065,260 V340"/><path d="M1385,260 V340"/>
<path d="M1385,340 H425 V264" marker-end="url(#r)"/>
</g>
<text x="905" y="384" text-anchor="middle" font-family="{FONT}" font-size="19" font-weight="700" fill="#d63a46">not met, the critic objects, or you say no: the reason goes back and a new session starts</text>
</svg>
'''
open(__import__('os').path.join(__import__('os').path.dirname(__file__), '..', 'example.html'),'w').write(html)
