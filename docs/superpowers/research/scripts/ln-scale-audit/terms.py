import sys, csv, numpy as np
def terms(path, label):
    rows=[r for r in csv.DictReader(open(path)) if r.get('phot')]
    f=lambda r,k: float(r[k])
    T={k:[] for k in ('libgap','bg','samp','ap','trut','dS2')}
    for r in rows:
        T['libgap'].append(np.log(f(r,'s_lib')/f(r,'phot')))
        T['bg'].append(np.log(f(r,'s_lib')/f(r,'s_ellsky_all')))
        T['samp'].append(np.log(f(r,'s_ellsky_all')/f(r,'e1sky_iso')))
        T['ap'].append(np.log(f(r,'e1sky_iso')/f(r,'c4_iso')))
        T['trut'].append(np.log(f(r,'c4_iso')/f(r,'phot')))
        T['dS2'].append((f(r,'fwhm_mem')/2.3548)**2-(f(r,'fwhm_ref')/2.3548)**2)
    T={k:np.array(v) for k,v in T.items()}
    g=T['libgap']
    print(f"== {label}: n {len(g)}; all terms in % (log ratios x100); identity libgap = bg + samp + ap + trut")
    print(f"   {'term':6s} {'median':>7s} {'mean':>7s} {'sd':>6s} {'r(term,gap)':>11s} {'slope vs dS2 %/px2':>18s}")
    for k in ('libgap','bg','samp','ap','trut'):
        v=T[k]*100
        r=np.corrcoef(v,g)[0,1] if k!='libgap' else 1.0
        sl=np.polyfit(T['dS2'],v,1)[0]
        print(f"   {k:6s} {np.median(v):7.2f} {v.mean():7.2f} {v.std():6.2f} {r:11.3f} {sl:18.2f}")
    pred=(T['bg']+T['ap'])*100
    b,a=np.polyfit(pred,g*100,1)
    print(f"   gap vs (bg+ap): slope {b:.3f} intercept {a:.2f} r {np.corrcoef(pred,g)[0,1]:.3f}; residual sd {np.std(g*100-pred):.2f} vs gap sd {np.std(g*100):.2f}")
for p,l in zip(sys.argv[1::2], sys.argv[2::2]): terms(p,l)
