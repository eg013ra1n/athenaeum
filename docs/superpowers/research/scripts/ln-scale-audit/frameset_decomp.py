"""Frame-set decomposition: probe JSON (comma-separated files) + truth JSON + per-star CSVs
+ frameset-frames.json (prepare_frameset.py) -> per-member terms CSV (ln-scale-audit)."""
import json, sys, numpy as np, csv
sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent))
from decompose import iso_mask
from stars import load, per_star, flux
frames={f"f{x['frame']}":x for x in json.load(open('frameset-frames.json'))}
probe={}
for f in sys.argv[1].split(','):
    for l in open(f):
        j=json.loads(l); probe[(j['label'],j['plane'])]=j
truth={}
for f in sys.argv[2].split(','):
    for l in open(f):
        j=json.loads(l); truth[(j['label'],j['plane'])]=j
stars_dir=sys.argv[3]; outcsv=sys.argv[4]
rows=[]
for key,j in sorted(probe.items(), key=lambda kv: truth.get(kv[0],{}).get('fwhm_mem',0)):
    t=truth.get(key); fr=frames[key[0]]
    if not t or not (t['ratio']==t['ratio']): continue
    a=load(f"{stars_dir}/{key[0]}.p{key[1]}.csv"); ref=load(f"{stars_dir}/reference.p{key[1]}.csv"); d=per_star(a)
    ok=(a["r_amp"]+a["r_b"]<0.5)&(a["t_amp"]+a["t_b"]<0.5)
    for k in ("prod","e1sky","c4","EFr","EFt"): ok&=np.isfinite(d[k])
    iso=iso_mask(a,ref,8*j['F'])&ok
    sel=np.argsort(-np.where(iso,d["bright"],-np.inf))[:min(150,int(iso.sum()))]
    athln=fr['athln'][key[1]]
    rows.append(dict(handle=int(key[0][1:]), plane=key[1], applied=athln, s_lib=j['sLib'], s_ellsky_all=j['sEllipseAnnulusSky'],
        e1sky_iso=float(np.median(d['e1sky'][sel])), c4_iso=float(np.median(d['c4'][sel])), c5_iso=float(np.median(d['c5'][sel])),
        EFr_iso=float(np.median(d['EFr'][sel])), EFt_iso=float(np.median(d['EFt'][sel])), n_iso=len(sel),
        cog_r=float(np.median((a["r_sig"]/flux(a,"r","c4",a["r_sky"]))[sel])), cog_t=float(np.median((a["t_sig"]/flux(a,"t","c4",a["t_sky"]))[sel])),
        phot=t['ratio'], se=t['se'], mode=t['mode'], fwhm_ref=t['fwhm_ref'], fwhm_mem=t['fwhm_mem'], fwhm_fit_mem=j['tgtFwhmFit'], fwhm_fit_ref=j['refFwhmFit'],
        F=j['F'], beta=j['beta'], diff_pct=(athln/t['ratio']-1)*100, weight=fr['weight']))
keys=list(rows[0].keys())
with open(outcsv,'w',newline='') as f:
    w=csv.DictWriter(f,fieldnames=keys); w.writeheader(); w.writerows(rows)
print(f"{'frame':>6} {'p':>1} {'mode':>7} {'FWm':>5} {'dS2':>5} | {'athln':>7} {'s_lib':>7} {'lib/ath%':>8} {'phot':>7} {'gap%':>6} | {'EFr':>5} {'EFt':>5} {'bg%':>5}")
for r in rows:
    ds2=(r['fwhm_mem']/2.3548)**2-(r['fwhm_ref']/2.3548)**2
    print(f"{r['handle']:>6} {r['plane']:>1} {r['mode']:>7} {r['fwhm_mem']:5.2f} {ds2:5.2f} | {r['applied']:7.4f} {r['s_lib']:7.4f} {(r['s_lib']/r['applied']-1)*100:8.3f} {r['phot']:7.4f} {r['diff_pct']:6.2f} | {r['EFr_iso']:5.3f} {r['EFt_iso']:5.3f} {(r['s_lib']/r['s_ellsky_all']-1)*100:5.2f}")
