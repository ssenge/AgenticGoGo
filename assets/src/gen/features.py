# Writes assets/src/features.html (the ring is computed here so its arcs clear the boxes); render with scripts/render_assets.sh.
import math
W,H=1700,880
cx,cy,R=500,445,292
FONT="-apple-system,Helvetica Neue,Arial,sans-serif"
def pos(a): 
    r=math.radians(a); return (cx+R*math.sin(r), cy-R*math.cos(r))
# stations: name, angle, w, h (diamond uses w as full width)
st={'task':(330,190,92),'agent':(30,320,200),'ver':(90,290,126),'met':(150,130,130),'retry':(210,350,112)}
rects={}
for k,(a,w,h) in st.items():
    x,y=pos(a); rects[k]=(x-w/2-10,y-h/2-10,x+w/2+10,y+h/2+10)
def inside(p,k):
    x0,y0,x1,y1=rects[k]; return x0<=p[0]<=x1 and y0<=p[1]<=y1
def arc(a1,a2,k1,k2,cw=True):
    # march along the ring from a1 to a2, trim by the boxes
    step=0.25; a=a1; pts=[]
    n=int(((a2-a1)%360)/step)
    seq=[(a1+i*step)%360 for i in range(n+1)]
    s=next(i for i,t in enumerate(seq) if not inside(pos(t),k1))
    e=max(i for i,t in enumerate(seq) if not inside(pos(t),k2))
    p1,p2=pos(seq[s]),pos(seq[e]); span=(seq[e]-seq[s])%360
    return f'M{p1[0]:.1f},{p1[1]:.1f} A{R},{R} 0 {1 if span>180 else 0} 1 {p2[0]:.1f},{p2[1]:.1f}'
fwd=[arc(330,30,'task','agent'),arc(30,90,'agent','ver'),arc(90,150,'ver','met')]
retry=[arc(150,210,'met','retry'),arc(210,330,'retry','task')]
def box(k,style,inner):
    a,w,h=st[k]; x,y=pos(a)
    return f'<div class="b" style="left:{x-w/2:.0f}px;top:{y-h/2:.0f}px;width:{w}px;height:{h}px;{style}">{inner}</div>'
html=f'''<!doctype html><meta charset="utf-8">
<style>
:root{{--agent:#2a9d8f;--verifier:#d63a46;--human:#f28c45;--engine:#264653;--ink:#1d2b33;--mute:#5b6b73}}
*{{box-sizing:border-box}}
html,body{{margin:0;background:#fff}}
body{{width:{W}px;height:{H}px;position:relative;font-family:{FONT};color:var(--ink)}}
.b{{position:absolute;border-radius:18px;display:flex;flex-direction:column;justify-content:center;align-items:center;text-align:center;padding:8px 14px;color:#fff}}
.b h4{{margin:0;font-size:28px;letter-spacing:.06em;font-weight:800}}
.b p{{margin:5px 0 0;font-size:17px;line-height:1.28}}
.b small{{display:block;margin-top:5px;font-size:15px;opacity:.92}}
.lane{{display:inline-block;width:72px;height:34px;line-height:34px;border-radius:8px;background:rgba(255,255,255,.22);border:1.5px solid rgba(255,255,255,.7);font-size:15px;font-weight:700;margin:0 4px}}
.lbl{{position:absolute;font-size:15px;font-weight:800;letter-spacing:.12em;color:var(--mute);text-transform:uppercase}}
.chip{{position:absolute;width:350px;height:62px;border-radius:12px;background:#fff;border:2px solid #d5e0e4;border-left-width:9px;display:flex;align-items:center;padding:0 16px;font-size:18px;font-weight:600;line-height:1.2}}
.cap{{position:absolute;text-align:center;font-weight:800}}
</style>
<svg width="{W}" height="{H}" style="position:absolute;left:0;top:0" xmlns="http://www.w3.org/2000/svg">
<defs>
<marker id="a" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#264653"/></marker>
<marker id="r" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#d63a46"/></marker>
</defs>
<circle cx="{cx}" cy="{cy}" r="{R}" fill="none" stroke="#e7eef0" stroke-width="22"/>
<g stroke="#264653" stroke-width="4.5" fill="none">{"".join(f'<path d="{d}" marker-end="url(#a)"/>' for d in fwd)}</g>
<g stroke="#d63a46" stroke-width="4.5" fill="none" stroke-dasharray="11 8">{"".join(f'<path d="{d}" marker-end="url(#r)"/>' for d in retry)}</g>
'''
# exit chain
yc=pos(150)[1]; mx=pos(150)[0]
html+=f'''<g stroke="#264653" stroke-width="4" fill="none" marker-end="url(#a)">
<path d="M{mx+68:.0f},{yc:.0f} H830" marker-end="url(#a)"/>
<path d="M1040,{yc:.0f} H1052"/><path d="M1204,{yc:.0f} H1256"/><path d="M1420,{yc:.0f} H1466"/>
</g>
<path d="M1130,{yc-74:.0f} V{yc-112:.0f} H1555 V{yc-47:.0f}" stroke="#264653" stroke-width="4" fill="none" stroke-dasharray="3 7" marker-end="url(#a)"/>
<polygon points="1130,{yc-74:.0f} 1204,{yc:.0f} 1130,{yc+74:.0f} 1056,{yc:.0f}" fill="#fff" stroke="#264653" stroke-width="4"/>
<text x="1130" y="{yc-4:.0f}" text-anchor="middle" font-family="{FONT}" font-size="22" font-weight="800" fill="#1d2b33">if …</text>
<text x="1130" y="{yc+22:.0f}" text-anchor="middle" font-family="{FONT}" font-size="16" font-weight="600" fill="#5b6b73">condition</text>
<text x="780" y="{yc-14:.0f}" text-anchor="middle" font-family="{FONT}" font-size="19" font-weight="800" fill="#264653">met</text>
<text x="1230" y="{yc-14:.0f}" text-anchor="middle" font-family="{FONT}" font-size="17" font-weight="800" fill="#264653">then</text>
<text x="1340" y="{yc-122:.0f}" text-anchor="middle" font-family="{FONT}" font-size="17" font-weight="800" fill="#264653">else: skip, or another branch</text>
<text x="338" y="{cy+8:.0f}" text-anchor="middle" font-family="{FONT}" font-size="17" font-weight="700" fill="#d63a46" transform="rotate(-90 232,{cy})"></text>
</svg>
'''
html+=box('task','background:#fff;color:var(--ink);border:2.5px solid #b9c8ce','<h4 style="font-size:22px;letter-spacing:.05em">TASK</h4><p style="font-size:15px">what to do, and what failed before</p>')
html+=box('agent','background:var(--agent)','<h4>AGENTS</h4><div style="margin-top:8px"><span class="lane">lane</span><span class="lane">lane</span><span class="lane">lane</span></div><small>in parallel: local · ssh · container</small><small style="margin-top:2px">Claude Code · Codex · Copilot · …</small>')
html+=box('ver','background:var(--verifier)','<h4>VERIFIERS</h4><p>scripts · rubrics · metrics · critic</p><small>run by Relentless itself:<br>agents can’t touch them</small>')
html+=box('retry','background:#fff;color:#a52632;border:3px dashed var(--verifier)','<h4 style="font-size:22px;letter-spacing:.05em;color:#d63a46">NOT MET: AGAIN</h4><p style="color:#a52632">the reason goes back<br>a stronger agent · the task force repairs</p>')
mxx,myy=pos(150)
html+=f'<svg width="{W}" height="{H}" style="position:absolute;left:0;top:0"><polygon points="{mxx:.0f},{myy-65:.0f} {mxx+65:.0f},{myy:.0f} {mxx:.0f},{myy+65:.0f} {mxx-65:.0f},{myy:.0f}" fill="#fff" stroke="#264653" stroke-width="4"/><text x="{mxx:.0f}" y="{myy-3:.0f}" text-anchor="middle" font-family="{FONT}" font-size="21" font-weight="800" fill="#1d2b33">all</text><text x="{mxx:.0f}" y="{myy+21:.0f}" text-anchor="middle" font-family="{FONT}" font-size="21" font-weight="800" fill="#1d2b33">met?</text></svg>'
html+=f'<div class="b" style="left:830px;top:{yc-48:.0f}px;width:210px;height:96px;background:#fdf1e8;color:#a8561a;border:3px solid var(--human)"><h4 style="font-size:22px;letter-spacing:.05em">HUMAN</h4><p style="margin-top:3px">approves where it matters</p></div>'
html+=f'<div class="b" style="left:1260px;top:{yc-48:.0f}px;width:160px;height:96px;background:#fff;color:var(--ink);border:2.5px solid #b9c8ce"><h4 style="font-size:21px;letter-spacing:.05em">NEXT STEPS</h4><p style="font-size:15px;margin-top:3px">any mix of these</p></div>'
html+=f'<div class="b" style="left:1470px;top:{yc-48:.0f}px;width:170px;height:96px;background:var(--engine)"><h4 style="font-size:26px">✔ DONE</h4></div>'
# center
html+=f'<img src="mark.png" style="position:absolute;left:{cx-105}px;top:{cy-150}px;width:210px;height:210px">'
html+=f'<div class="cap" style="left:{cx-150}px;top:{cy+66}px;width:300px;font-size:30px;letter-spacing:.04em;color:var(--engine)">Relentless</div>'
html+=f'<div class="cap" style="left:{cx-150}px;top:{cy+108}px;width:300px;font-size:17px;font-weight:600;color:var(--mute);line-height:1.3">drives the loop and records<br>every call: resume from anywhere</div>'
# legend
html+='<div class="lbl" style="left:960px;top:70px">A workflow can</div>'
chips=[("loop until the job is verified","#2a9d8f"),("branch with ifs and conditions","#264653"),("run steps and items in parallel lanes","#2a9d8f"),("run lanes remotely: ssh, container, cloud VM","#2a9d8f"),("wait for a human, then carry on","#f28c45"),("try a stronger agent, repair, or escalate","#d63a46")]
for i,(t,c) in enumerate(chips):
    col,row=i%2,i//2
    html+=f'<div class="chip" style="left:{960+col*370}px;top:{108+row*82}px;border-left-color:{c}">{t}</div>'
open(__import__('os').path.join(__import__('os').path.dirname(__file__), '..', 'features.html'),'w').write(html)
