#!/usr/bin/env python3
"""Reads results_*.json, writes tables (tables.md) and plots (png)."""
import json, math, os
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

SCEN_SH = {"40/30/20/10": [40, 30, 20, 10], "60/20/20": [60, 20, 20], "25x4": [25] * 4, "10x10": [10] * 10, "40+30x2": [40] + [2] * 30}
OUT = []
T975 = {1:12.706,2:4.303,3:3.182,4:2.776,5:2.571,6:2.447,7:2.365,8:2.306,9:2.262,10:2.228}


def tc(df):
    return T975.get(df, 1.96)


def load(g):
    p = f"results_{g}.json"
    if not os.path.exists(p):
        return {}
    return {tuple(json.loads(k)): v for k, v in json.load(open(p)).items()}


def ci(x):
    x = np.asarray(x, float)
    if len(x) < 2:
        return float(x.mean(0)), 0.0
    return x.mean(0), tc(len(x) - 1) * x.std(0, ddof=1) / math.sqrt(len(x))


def top(runs, key="share", i=0):
    return ci([r[key][i] for r in runs])


def comp(sn, i=0):
    s = np.array(SCEN_SH[sn], float)
    return s[i] / s.sum()


def fmt(m, c, pct=True, d=1):
    return f"{100*m:.{d}f}±{100*c:.{d}f}" if pct else f"{m:.{d}f}±{c:.{d}f}"


def mean(runs, k):
    return float(np.mean([r[k] for r in runs]))


def table(title, head, rows):
    OUT.append(f"\n**{title}**\n\n| " + " | ".join(head) + " |\n|" + "---|" * len(head))
    for r in rows:
        OUT.append("| " + " | ".join(str(x) for x in r) + " |")
    OUT.append("")


def excess(runs, sn, i=0, key="share"):
    m, c = top(runs, key, i)
    a = comp(sn, i)
    return m - a, c


# ---------------------------------------------------------------- grid
G = load("grid")
Cs = sorted({k[1] for k in G})
if G:
    for sn in SCEN_SH:
        rows = []
        for C in Cs:
            for H in [0, 60, 120, 300]:
                runs = G[(sn, C, H, "auction")]
                e, c = excess(runs, sn)
                a = comp(sn)
                sw = mean(runs, "same_winner"); sw0 = mean(runs, "same_winner_expected")
                rows.append([C, H, f"{100*top(runs)[0]:.1f}", f"{100*e:+.2f}±{100*c:.2f}", f"{100*e/a:+.1f}%", f"{mean(runs,'W_mean'):.0f}", f"{mean(runs,'W_pos'):.2f}", f"{100*sw:.1f} ({100*sw0:.1f})", f"{mean(runs,'blocktime'):.0f}", f"{100*mean(runs,'stale'):.1f}"])
        table(f"Grid {sn}: top miner ({100*comp(sn):.0f}% compute)", ["C (units)", "H (s)", "block share %", "excess pp ±CI", "rel excess", "mean W (s)", "P(W>1s)", "prev-winner wins % (expected)", "block time (s)", "stale %"], rows)
    # plot excess vs C for each scenario, H=120 and 300
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2), sharey=True)
    cols = {"40/30/20/10": "#1f77b4", "60/20/20": "#d62728", "25x4": "#2ca02c", "10x10": "#7f7f7f", "40+30x2": "#9467bd"}
    for ax, H in zip(axs, [120, 300]):
        for sn in SCEN_SH:
            ms = [excess(G[(sn, C, H, "auction")], sn) for C in Cs]
            ax.errorbar(Cs, [100 * m for m, _ in ms], yerr=[100 * c for _, c in ms], label=sn, color=cols[sn], marker="o", ms=3, capsize=2)
        ax.axhline(0, color="k", lw=.6); ax.set_xscale("log"); ax.set_title(f"commit lead H = {H} s"); ax.set_xlabel("network compute C (web-miner machines)")
    axs[0].set_ylabel("#1 miner block share minus compute share (pp)"); axs[0].legend(fontsize=7)
    plt.tight_layout(); plt.savefig("excess_vs_competition.png", dpi=130); plt.close()
    # excess vs H at several C
    fig, ax = plt.subplots(figsize=(6, 4.2))
    for C in [3, 30, 300]:
        ms = [excess(G[("40/30/20/10", C, H, "auction")], "40/30/20/10") for H in [0, 60, 120, 300]]
        ax.errorbar([0, 60, 120, 300], [100 * m for m, _ in ms], yerr=[100 * c for _, c in ms], marker="o", label=f"C={C}", capsize=2)
    ax.axhline(0, color="k", lw=.6); ax.set_xlabel("H: how long before validity a miner commits to its held run (s)"); ax.set_ylabel("#1 (40%) excess, pp"); ax.legend(); plt.tight_layout(); plt.savefig("excess_vs_H.png", dpi=130); plt.close()
    # W vs C
    fig, ax = plt.subplots(figsize=(6, 4.2))
    for H in [0, 120]:
        ax.plot(Cs, [mean(G[("40/30/20/10", C, H, "auction")], "W_mean") for C in Cs], marker="o", label=f"mean W of winner (H={H})")
    ax.plot(Cs, [mean(G[("40/30/20/10", C, 120, "auction")], "blocktime") for C in Cs], marker="s", label="block time")
    ax.set_xscale("log"); ax.set_xlabel("C"); ax.set_ylabel("seconds"); ax.legend(); plt.tight_layout(); plt.savefig("W_vs_competition.png", dpi=130); plt.close()
    # rule-hook check
    rows = []
    for sn in ["40/30/20/10", "40+30x2"]:
        for C in [3, 30, 300]:
            a = excess(G[(sn, C, 120, "auction")], sn); b = excess(G[(sn, C, 120, "at-publish")], sn); z = excess(G[(sn, C, 0, "auction")], sn)
            rows.append([sn, C, f"{100*a[0]:+.2f}±{100*a[1]:.2f}", f"{100*z[0]:+.2f}±{100*z[1]:.2f}", f"{100*b[0]:+.2f}±{100*b[1]:.2f}"])
    table("Rule hook check: top-miner excess (pp) with H=120 at-find vs H=0 vs at-publish rule", ["scenario", "C", "at-find H=120", "at-find H=0", "at-publish (H=120)"], rows)
    # all miners
    rows = []
    for sn in ["40/30/20/10", "60/20/20", "25x4"]:
        for C in [3, 300]:
            runs = G[(sn, C, 300, "auction")]
            sh = SCEN_SH[sn]; n = len(sh)
            rows.append([sn, C] + [f"{100*top(runs,'share',i)[0]:.1f} vs {100*comp(sn,i):.0f}" for i in range(n)] + [""] * (4 - n))
    table("Per-miner block share vs compute share, H=300", ["scenario", "C", "m1", "m2", "m3", "m4"], rows)
    # Markov check
    rows = []
    for sn in ["40/30/20/10", "60/20/20", "25x4", "10x10", "40+30x2"]:
        for C in [30, 300]:
            for H in [120, 300]:
                runs = G[(sn, C, H, "auction")]
                a = comp(sn)
                s_ = top(runs)[0]
                pp = float(np.mean([r["p_after_win"][0] for r in runs])); pm = float(np.mean([r["p_after_loss"][0] for r in runs]))
                dl = (pp - a) / (1 - a) if a < 1 else 0
                memo = a / (1 - dl * (1 - a)) if dl * (1 - a) < 1 else float("nan")
                exact = pm / (1 - pp + pm)
                rows.append([sn, C, H, f"{100*a:.0f}", f"{100*pp:.1f}", f"{100*pm:.1f}", f"{dl:.3f}", f"{100*memo:.1f}", f"{100*exact:.1f}", f"{100*s_:.1f}"])
    table("Check of s = alpha/(1-delta(1-alpha)) for the #1 miner (alpha: compute share; p+ = P(win | won previous); p- = P(win | lost previous); delta = (p+-alpha)/(1-alpha); memo uses p- = alpha; exact is the 2-state Markov stationary value)", ["scenario", "C", "H", "alpha %", "p+ %", "p- %", "delta", "memo s %", "Markov s %", "measured s %"], rows)

# ---------------------------------------------------------------- deviations
D = load("dev")
if D:
    rows = []
    for (sn, C, who, H), runs in sorted(D.items(), key=lambda kv: (kv[0][0], kv[0][1], kv[0][2], kv[0][3])):
        n = len(SCEN_SH[sn]); i = 0 if who == 0 else n - 1
        m, c = top(runs, "share", i); a = comp(sn, i)
        rows.append([sn, C, "#1" if who == 0 else "smallest", H, f"{100*a:.1f}", f"{100*m:.2f}±{100*c:.2f}", f"{m/a:.3f}"])
    table("Unilateral deviation: one miner commits at lead H, everyone else H=0 (no head start)", ["scenario", "C", "who", "H", "compute %", "block share %", "share/compute"], rows)

# ---------------------------------------------------------------- kappa
K = load("kappa")
if K and G:
    rows = []
    for C in Cs:
        for H in [120, 300]:
            a = excess(K[("40/30/20/10", C, H)], "40/30/20/10"); b = excess(G[("40/30/20/10", C, H, "auction")], "40/30/20/10")
            rows.append([C, H, f"{100*b[0]:+.2f}±{100*b[1]:.2f}", f"{100*a[0]:+.2f}±{100*a[1]:.2f}", f"{mean(K[('40/30/20/10',C,H)],'W_mean'):.0f}"])
    table("Sensitivity: #1 excess (pp) with iid dots (kappa=0) vs search depth improving dots (kappa=0.3)", ["C", "H", "kappa=0", "kappa=0.3", "mean W (kappa 0.3)"], rows)

# ---------------------------------------------------------------- selfish
S = load("selfish")
if S:
    rows = []
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2), sharey=True)
    for ax, C in zip(axs, [10, 100]):
        al = sorted({k[2] for k in S if k[1] == C})
        ax.plot([0, .5], [0, .5], "k:", lw=.8, label="fair")
        for lab, key, g, Hh, col in [("honest H=0", "honest", 0, 0, "gray"), ("honest, head start H=120", "honest", 0, 120, "k"),
                                      ("selfish g=0, H=0", "selfish", 0.0, 0, "#1f77b4"), ("selfish g=0.5, H=0", "selfish", 0.5, 0, "#2ca02c"), ("selfish g=1, H=0", "selfish", 1.0, 0, "#d62728"),
                                      ("selfish g=0.5, H=120", "selfish", 0.5, 120, "#ff7f0e"), ("selfish g=0.5, at-publish rule", "selfish-at-publish", 0.5, 120, "#9467bd")]:
            ys = []
            for a in al:
                kk = (key, C, a, g, Hh) if key != "honest" else (key, C, a, 0, Hh)
                runs = S.get(kk)
                ys.append(top(runs)[0] if runs else np.nan)
            ax.plot(al, ys, marker="o", ms=3, label=lab, color=col)
        ax.set_title(f"C={C}"); ax.set_xlabel("selfish miner compute share")
    axs[0].set_ylabel("its block share on the main chain"); axs[0].legend(fontsize=7)
    plt.tight_layout(); plt.savefig("selfish.png", dpi=130); plt.close()
    for C in [10, 100]:
        al = sorted({k[2] for k in S if k[1] == C})
        for a in al:
            row = [C, a]
            for key, g, Hh in [("honest", 0, 0), ("honest", 0, 120), ("selfish", 0.0, 0), ("selfish", 0.5, 0), ("selfish", 1.0, 0), ("selfish", 0.0, 120), ("selfish", 0.5, 120), ("selfish", 1.0, 120), ("selfish-at-publish", 0.5, 120)]:
                kk = (key, C, a, g if key != "honest" else 0, Hh)
                runs = S.get(kk)
                row.append(fmt(*top(runs)) if runs else "")
            rows.append(row)
    table("Selfish miner (miner 1 with share alpha, 5 equal honest others): block share %, mean±CI. Columns: honest(H=0), honest(H=120), selfish H=0 g=0/.5/1, selfish H=120 g=0/.5/1, selfish under at-publish rule",
          ["C", "alpha", "honest H0", "honest H120", "s H0 g0", "s H0 g.5", "s H0 g1", "s H120 g0", "s H120 g.5", "s H120 g1", "at-publish"], rows)
    # thresholds: smallest alpha where selfish > honest baseline with the same H, by interpolation
    rows = []
    for C in [10, 100]:
        al = sorted({k[2] for k in S if k[1] == C})
        for g in [0.0, 0.5, 1.0]:
            for Hh in [0, 120]:
                gains = []
                for a in al:
                    s_ = S.get(("selfish", C, a, g, Hh)); h_ = S.get(("honest", C, a, 0, Hh))
                    ds = [x["share"][0] - y["share"][0] for x, y in zip(s_, h_)]
                    gains.append((a, np.mean(ds), tc(len(ds) - 1) * np.std(ds, ddof=1) / math.sqrt(len(ds))))
                thr = next((a for a, d, c in gains if d - c > 0), None)
                rows.append([C, g, Hh, " ".join(f"{a}:{100*d:+.1f}" for a, d, c in gains), thr if thr is not None else ">0.45"])
    table("Selfish minus honest share (pp) at each alpha; threshold = smallest alpha with a significantly positive gain", ["C", "gamma", "H", "gain by alpha (pp)", "threshold alpha"], rows)

# ---------------------------------------------------------------- A dial
A = load("A")
if A:
    dhs = [30, 60, 120, 300, 1000, 3000, "flat"]
    rows = []
    for sn in ["40/30/20/10", "60/20/20", "40+30x2"]:
        for C in [3, 30, 300]:
            for H in ([0, 120, 300] if sn == "40/30/20/10" else [120]):
                for dh in dhs:
                    runs = A[(sn, C, H, dh)]
                    e, c = excess(runs, sn)
                    rows.append([sn, C, H, dh, f"{100*e:+.2f}±{100*c:.2f}", f"{mean(runs,'W_mean'):.0f}", f"{mean(runs,'W_pos'):.2f}", f"{mean(runs,'F_mean'):.0f}", f"{mean(runs,'blocktime'):.0f}", f"{100*mean(runs,'stale'):.1f}", f"{100*mean(runs,'same_winner'):.1f} ({100*mean(runs,'same_winner_expected'):.1f})"])
    table("A: slope dial (seconds per difficulty halving; 'flat' = valid on find). Monero retarget, start_halvings=0. F = mean winning run length", ["scenario", "C", "H", "dh", "#1 excess pp", "W (s)", "P(W>1)", "F (frames)", "block time", "stale %", "prev-winner % (exp)"], rows)
    rows = []
    for C in [0.3, 3, 30, 300]:
        runs = A[("40/30/20/10", C, 120, "today")]
        e, c = excess(runs, "40/30/20/10")
        rows.append([C, f"{100*e:+.2f}±{100*c:.2f}", f"{mean(runs,'W_mean'):.0f}", f"{mean(runs,'W_pos'):.2f}", f"{mean(runs,'F_mean'):.0f}", f"{mean(runs,'blocktime'):.0f}"])
    table("Today's constants (start_halvings=20, 60 s/halving), H=120", ["C", "excess pp", "W", "P(W>1)", "F", "block time"], rows)
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2))
    x = list(range(len(dhs)))
    for C in [3, 30, 300]:
        runsl = [A[("40/30/20/10", C, 300, dh)] for dh in dhs]
        axs[0].errorbar(x, [100 * excess(r, "40/30/20/10")[0] for r in runsl], yerr=[100 * excess(r, "40/30/20/10")[1] for r in runsl], marker="o", label=f"C={C}", capsize=2)
        axs[1].plot(x, [mean(r, "F_mean") for r in runsl], marker="o", label=f"C={C}")
    for ax in axs:
        ax.set_xticks(x); ax.set_xticklabels([str(d) for d in dhs]); ax.set_xlabel("limit slope: seconds per halving (flat = valid on find)")
    axs[0].set_ylabel("#1 (40%) excess pp, H=300"); axs[1].set_ylabel("mean winning run length (frames)"); axs[0].legend()
    axs[0].axhline(0, color="k", lw=.6)
    plt.tight_layout(); plt.savefig("A_dial.png", dpi=130); plt.close()
    fig, ax = plt.subplots(figsize=(6, 4.2))
    for C in [3, 30, 300]:
        runsl = [A[("40/30/20/10", C, 120, dh)] for dh in dhs]
        ax.plot(x, [mean(r, "W_mean") for r in runsl], marker="o", label=f"C={C}")
    ax.set_xticks(x); ax.set_xticklabels([str(d) for d in dhs]); ax.set_ylabel("mean W of winning block (s)"); ax.set_xlabel("seconds per halving"); ax.legend()
    plt.tight_layout(); plt.savefig("A_W.png", dpi=130); plt.close()

# ---------------------------------------------------------------- F
F = load("F")
if F:
    rows = []
    for C in [3, 30, 300]:
        for H in [0, 120, 300]:
            for B in [300, 600, 1200]:
                runs = F[("40/30/20/10", C, H, B)]
                e, c = excess(runs, "40/30/20/10")
                rows.append([C, H, B, f"{100*e:+.2f}±{100*c:.2f}", f"{mean(runs,'W_mean'):.0f}", f"{mean(runs,'W_mean')/mean(runs,'blocktime'):.2f}", f"{mean(runs,'blocktime'):.0f}", f"{mean(runs,'F_mean'):.0f}"])
    table("F: target block time sweep (today's slope)", ["C", "H", "target B", "#1 excess pp", "W (s)", "W / block time", "block time", "F"], rows)

# ---------------------------------------------------------------- C
Cg = load("C")
if Cg:
    rows = []
    for sn in ["40/30/20/10", "40+30x2"]:
        for C in [3, 30, 300]:
            for lab in [("first", 0), ("shortest", 15), ("shortest", 30), ("shortest", 60), ("shortest", 120)]:
                runs = Cg[(sn, C) + lab]
                e, c = excess(runs, sn)
                rows.append([sn, C, lab[0], lab[1], f"{100*e:+.2f}±{100*c:.2f}", f"{mean(runs,'W_mean'):.0f}", f"{mean(runs,'F_mean'):.0f}", f"{mean(runs,'blocktime'):.0f}", f"{100*mean(runs,'stale'):.1f}", f"{100*mean(runs,'same_winner'):.1f} ({100*mean(runs,'same_winner_expected'):.1f})"])
    table("C: valid on find (flat cap), tie rule first-seen vs shortest-run-within-omega. Honest miners", ["scenario", "C", "tie rule", "omega", "#1 excess pp", "W", "F", "block time", "stale/displaced %", "prev-winner % (exp)"], rows)
CS_ = load("Cselfish")
if CS_:
    rows = []
    al = sorted({k[2] for k in CS_ if k[0] == "honest"})
    for a in al:
        row = [a, fmt(*top(CS_[("honest", 30, a)]))]
        for key in [("C-first", 30, a, 0.0, 0), ("C-first", 30, a, 0.5, 0), ("C-first", 30, a, 1.0, 0), ("C-shortest", 30, a, 0.0, 30), ("C-shortest", 30, a, 0.0, 60)]:
            row.append(fmt(*top(CS_[key])))
        rows.append(row)
    table("C + selfish miner (C=30 units): block share %", ["alpha", "honest", "first-seen g=0", "g=.5", "g=1", "shortest w=30", "shortest w=60"], rows)
    fig, ax = plt.subplots(figsize=(6, 4.2))
    ax.plot([0, .5], [0, .5], "k:", lw=.8)
    for lab, key, col in [("first-seen g=0", lambda a: ("C-first", 30, a, 0.0, 0), "#1f77b4"), ("first-seen g=.5", lambda a: ("C-first", 30, a, 0.5, 0), "#2ca02c"), ("first-seen g=1", lambda a: ("C-first", 30, a, 1.0, 0), "#d62728"),
                          ("shortest w=30", lambda a: ("C-shortest", 30, a, 0.0, 30), "#ff7f0e"), ("shortest w=60", lambda a: ("C-shortest", 30, a, 0.0, 60), "#9467bd")]:
        ax.plot(al, [top(CS_[key(a)])[0] for a in al], marker="o", ms=3, label=lab, color=col)
    ax.set_xlabel("selfish compute share"); ax.set_ylabel("its block share"); ax.legend(fontsize=7); plt.tight_layout(); plt.savefig("C_selfish.png", dpi=130); plt.close()

# ---------------------------------------------------------------- E
E = load("E")
if E:
    rows = []
    for sn in ["40/30/20/10", "40+30x2"]:
        for C in [3, 30, 300]:
            for var in ["S0", "C-30"]:
                row = [sn, C, var]
                for u in [0.0, 0.5, 0.875]:
                    runs = E[(sn, C, var, u)]
                    m, c = top(runs, "reward_share"); row.append(f"{100*(m-comp(sn)):+.2f}±{100*c:.2f}")
                rows.append(row)
    table("E: uncle rewards. #1 reward share minus compute share (pp), u = uncle reward fraction (u=0 is the baseline)", ["scenario", "C", "base", "u=0", "u=0.5", "u=0.875"], rows)
    # analytic bonus
    G1 = G
    if G1:
        rows = []
        for C in [3, 30, 300]:
            e = excess(G1[("40/30/20/10", C, 300, "auction")], "40/30/20/10")[0]
            rows.append([C, f"{100*e:+.2f}"] + [f"{100*b*e:+.2f}" for b in [0.5, 0.1, 0.0]])
        table("E2 (shares, analytic): reward = b*block + (1-b)*pro-rata shares, shares fair by construction, so excess scales with b. Base: S0, H=300, #1 excess pp", ["C", "b=1 (today)", "b=0.5", "b=0.1", "b=0"], rows)

# ---------------------------------------------------------------- ctrl
Ct = load("ctrl")
if Ct:
    rows = []
    for sn in ["40/30/20/10", "40+30x2"]:
        for C in [3, 30, 300]:
            for H in [0, 120, 300]:
                row = [sn, C, H]
                for v in ["S0", "S0-early", "S0-backdate", "A300", "A300-backdate"]:
                    runs = Ct[(sn, C, H, v)]
                    e, c = excess(runs, sn)
                    row.append(f"{100*e:+.2f}±{100*c:.2f} / bt {mean(runs,'blocktime'):.0f}")
                rows.append(row)
    table("Controls: #1 excess pp / block time. S0 = today; S0-early = everyone announces held runs at find and others build on them (upper bound of publication); S0-backdate = stamp T_P+g(F) instead of validity time; A300 = 300 s/halving, start_halvings 0", ["scenario", "C", "H", "S0", "S0-early", "S0-backdate", "A300", "A300-backdate"], rows)


# ---------------------------------------------------------------- fresh
Fr_ = load("fresh")
if Fr_:
    rows = []
    for sn in ["40/30/20/10", "40+30x2"]:
        for var in ["S0", "A300"]:
            for C in [0.3, 3, 30]:
                base = Fr_[(sn, var, C, "none", "nobound", "-")]
                b0 = top(base)[0]
                for bd in ["top", "all"]:
                    for D, mode in [("nobound", "-")] + [(d, m) for d in [30, 60, 120] for m in ["full", "aware"]]:
                        runs = Fr_[(sn, var, C, bd, D, mode)]
                        m_, c_ = top(runs)
                        extra = f"{100*(m_-b0):+.2f}±{100*c_:.2f}" if bd == "top" else "n/a"
                        rows.append([sn, var, C, bd, D, mode, extra, f"{mean(runs,'bd_gain'):.1f}", f"{mean(runs,'bd_frac'):.2f}", f"{100*mean(runs,'false_stale'):.3f}", f"{mean(runs,'blocktime'):.0f} / {mean(runs,'real_blocktime'):.0f}", f"{100*mean(runs,'stale'):.1f}"])
    table("Freshness bound vs back-dating (propagation 2 s + Exp(mean 3 s)). 'top' = only the #1 miner back-dates, 'all' = everybody. extra = #1 share minus its share when nobody back-dates (same config, no bound). gain = mean seconds the child's clock starts early (stamp vs real validity time) on the back-dater's blocks, frac = share of its blocks back-dated >1 s. false stale = honest blocks dropped as stale. bt = stamped / real block time. full = stamp T_P+g(F); aware = never older than bound minus 10 s", ["scenario", "rules", "C", "back-daters", "bound D (s)", "mode", "#1 extra pp", "gain s", "frac", "false stale %", "block time stamped/real", "stale %"], rows)
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2), sharey=True)
    for ax, var in zip(axs, ["S0", "A300"]):
        for C in [0.3, 3, 30]:
            b0 = top(Fr_[("40/30/20/10", var, C, "none", "nobound", "-")])[0]
            xs = ["none", 120, 60, 30]; ys = []
            for D in xs:
                runs = Fr_[("40/30/20/10", var, C, "top", "nobound" if D == "none" else D, "-" if D == "none" else "full")]
                ys.append(100 * (top(runs)[0] - b0))
            ax.plot(range(4), ys, marker="o", label=f"C={C} full back-dating")
        ax.set_xticks(range(4)); ax.set_xticklabels([str(x) for x in xs]); ax.set_title(var); ax.set_xlabel("freshness bound (s)"); ax.axhline(0, color="k", lw=.6)
    axs[0].set_ylabel("#1 extra block share from back-dating (pp)"); axs[0].legend(fontsize=7)
    plt.tight_layout(); plt.savefig("fresh_bound.png", dpi=130); plt.close()

open("tables.md", "w").write("\n".join(OUT))
print("ok", len(OUT))
