import sys, csv, numpy as np
stars, frames, allcsv = sys.argv[1], sys.argv[2], sys.argv[3]
S={k:[] for k in ('ref','master','sim_cur','sim_cor','sim_cur_norej','sim_cor_norej')}
for r in csv.DictReader(open(stars)):
    for k in S: S[k].append(float(r[k]))
S={k:np.array(v) for k,v in S.items()}
ok=np.all([np.isfinite(v)&(v>0) for v in S.values()],axis=0)
print(f"stars {ok.sum()} of {len(ok)}; flux ratio LN reference / master window (median, bootstrap SE):")
rng=np.random.default_rng(1)
def med_se(x):
    idx=rng.integers(0,len(x),(400,len(x))); return np.median(x), np.std(np.median(x[idx],axis=1))
for k in ('master','sim_cur','sim_cor','sim_cur_norej','sim_cor_norej'):
    m,se=med_se(S['ref'][ok]/S[k][ok]); print(f"   ref/{k:14s} {m:.4f} +- {se:.4f}")
for a,b in (('sim_cor','sim_cur'),('sim_cur_norej','sim_cur'),('sim_cor_norej','sim_cor')):
    m,se=med_se(S[a][ok]/S[b][ok]); print(f"   {a}/{b}: {m:.4f} +- {se:.4f}")
# brightness dependence
q=np.quantile(S['ref'][ok],[0,0.5,0.9,1])
for lo,hi in zip(q[:-1],q[1:]):
    m=ok&(S['ref']>=lo)&(S['ref']<=hi)
    print(f"   ref flux {lo:.3g}-{hi:.3g}: n {m.sum()} ref/cur {np.median(S['ref'][m]/S['sim_cur'][m]):.4f} ref/cor {np.median(S['ref'][m]/S['sim_cor'][m]):.4f} cur/norej {np.median(S['sim_cur'][m]/S['sim_cur_norej'][m]):.4f} cor/norej {np.median(S['sim_cor'][m]/S['sim_cor_norej'][m]):.4f}")
G={int(r['handle']):r for r in csv.DictReader(open(allcsv))}
F=list(csv.DictReader(open(frames)))
rows=[]
for r in F:
    h=int(r['label'][1:]); g=G.get(h)
    fw=float(g['fwhm_mem']) if g else 5.97
    gap=(float(g['applied'])/float(g['phot'])-1)*100 if g else np.nan
    cc=lambda a,b: float(r[a])/max(float(r[b]),1)*100
    rows.append((fw, gap, float(r['weight']), cc('cur_core_rej','cur_core_n'), cc('cor_core_rej','cor_core_n'), cc('cur_wing_rej','cur_wing_n'), cc('cor_wing_rej','cor_wing_n')))
a=np.array(rows)
print("\nper-frame rejected fraction inside star windows (core r<=F_ref, wing F_ref<r<=3F_ref), median by FWHM class, current vs corrected scale:")
print(f"   {'FWHM':>9s} {'n':>3s} {'gap%':>6s} {'weight':>6s} | {'core cur':>8s} {'core cor':>8s} | {'wing cur':>8s} {'wing cor':>8s}")
for lo,hi in ((0,2.9),(2.9,3.6),(3.6,4.4),(4.4,4.9),(4.9,7)):
    m=(a[:,0]>=lo)&(a[:,0]<hi)
    if m.sum(): print(f"   {lo:4.1f}-{hi:4.1f} {m.sum():3d} {np.nanmedian(a[m,1]):6.2f} {np.median(a[m,2]):6.3f} | {np.median(a[m,3]):8.2f} {np.median(a[m,4]):8.2f} | {np.median(a[m,5]):8.2f} {np.median(a[m,6]):8.2f}")
print(f"   all {len(a)}: core cur {np.mean(a[:,3]):.2f} cor {np.mean(a[:,4]):.2f}; wing cur {np.mean(a[:,5]):.2f} cor {np.mean(a[:,6]):.2f} (means)")
okg=np.isfinite(a[:,1])
print(f"   corr(core rejected cur, gap) {np.corrcoef(a[okg,1],a[okg,3])[0,1]:.3f}; corr(core rejected cor, gap) {np.corrcoef(a[okg,1],a[okg,4])[0,1]:.3f}")
# estimator arms (ln_scale_probe --stack-sim with `arms` in the config): median +- bootstrap SE of
# ref/arm with and without rejection, beside the truth-corrected no-rejection arm; L-22: the ratio of each
# arm's no-rejection flux to the truth arm's and to the LN reference's
fixed = {'sim_cur_norej', 'sim_cor_norej'}
with open(stars) as fh:
    header = next(csv.reader(fh))
arms = [k[4:-6] for k in header if k.startswith('sim_') and k.endswith('_norej') and k not in fixed]
if arms:
    A = {a: {v: [] for v in ('rej', 'norej')} for a in arms}
    for r in csv.DictReader(open(stars)):
        for a in arms:
            A[a]['rej'].append(float(r[f'sim_{a}'])); A[a]['norej'].append(float(r[f'sim_{a}_norej']))
    A = {a: {v: np.array(x) for v, x in d.items()} for a, d in A.items()}
    oka = ok.copy()
    for a in arms:
        for v in ('rej', 'norej'):
            oka &= np.isfinite(A[a][v]) & (A[a][v] > 0)
    print(f"\nestimator arms over {oka.sum()} stars: flux ratio LN reference / arm (median, bootstrap SE); truth arm for comparison:")
    m, se = med_se(S['ref'][oka] / S['sim_cor_norej'][oka])
    print(f"   {'truth (sim_cor_norej)':26s} ref/arm_norej {m:.4f} +- {se:.4g}")
    for a in arms:
        m1, s1 = med_se(S['ref'][oka] / A[a]['rej'][oka])
        m2, s2 = med_se(S['ref'][oka] / A[a]['norej'][oka])
        m3, s3 = med_se(A[a]['norej'][oka] / S['sim_cor_norej'][oka])
        print(f"   {a:26s} ref/arm {m1:.4f} +- {s1:.4g}   ref/arm_norej {m2:.4f} +- {s2:.4g}   arm_norej/sim_cor_norej {m3:.4f} +- {s3:.4g}   (arm_norej/ref {1 / m2:.4f})")
