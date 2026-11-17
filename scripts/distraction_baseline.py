#!/usr/bin/env python3
"""Agent-distraction baseline over the flare-insights index (item #869).

Replicates the method of Google's "Agent Distraction" post (2026-11-06) on
local session data: how often do agents see TODO/FIXME refs, act on them
unprompted, and what do those detours cost?  Read-only; stdlib only.

  python -I scripts/distraction_baseline.py [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--json]

--since/--until bound sessions.started_at; run once before and once after the
scope guard ships and compare the unprompted-detour rate.
"""
import argparse, json, math, os, random, re, sqlite3, statistics, sys
from collections import defaultdict

INSIGHTS = os.path.expanduser("~/.local/share/agentflare/insights/observatory.db")
BACKEND = os.path.expanduser("~/.agentflare/backend.db")
READ_TOOLS = re.compile(r"(^|__)(Read|ctx_read|ctx_search|ctx_compose|Grep|ctx_shell)$|^Bash$")
# Our stale refs are `item #N` mentions in comments/docs (e.g. "Item #71/#438"),
# plus Google-style TODO(#N)/FIXME(#N). Bare `#N` is skipped: PRs and headings.
REF = re.compile(r"(?:\bitems?\s*#|\b(?:TODO|FIXME)\s*\(\s*#?)(\d{1,4})\b", re.I)
ITEMISH = re.compile(r"item|git log|gh (issue|pr)|ctx_search|Grep")


def ro(path):
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def item_states():
    """sequence_id -> state group, for the project with the most items."""
    if not os.path.exists(BACKEND):
        return {}
    c = ro(BACKEND)
    pid = c.execute("select project_id from items group by 1 order by count(*) desc limit 1").fetchone()
    if not pid:
        return {}
    q = ("select i.sequence_id, s.group_name from items i join states s on s.id=i.state_id "
         "where i.project_id=? and i.deleted_at is null")
    return dict(c.execute(q, (pid[0],)).fetchall())


def sessions(c, since, until):
    q = "select id, project, model, tool_call_count, duration_secs, input_tokens+output_tokens+cache_read_tokens+cache_write_tokens from sessions where 1=1"
    args = []
    if since:
        q += " and started_at >= ?"; args.append(since)
    if until:
        q += " and started_at < ?"; args.append(until)
    return c.execute(q, args).fetchall()


def analyse(c, sess, states):
    rows = []
    for sid, project, model, calls, dur, toks in sess:
        user = " ".join(t or "" for (t,) in c.execute("select user_text from turns where session_id=?", (sid,)))
        tcs = c.execute("select turn_seq,name,input,output from tool_calls where session_id=? order by created_at, rowid", (sid,)).fetchall()
        reads = [t for t in tcs if READ_TOOLS.search(t[1])]
        exposed = {}  # id -> index of first exposure in tcs
        for idx, (_, name, _, out) in enumerate(tcs):
            if out and READ_TOOLS.search(name):
                for m in REF.finditer(out):
                    exposed.setdefault(m.group(1), idx)
        acted, unprompted = set(), set()
        for ref, idx in exposed.items():
            pat = re.compile(rf"(?<!\d)#?{ref}(?!\d)")
            for name, inp in ((t[1], t[2]) for t in tcs[idx + 1:]):
                if ITEMISH.search(name + " " + (inp or "")[:200]) and pat.search(inp or ""):
                    acted.add(ref)
                    break
            if ref in acted and not re.search(rf"(?<!\d)#?{ref}(?!\d)", user):
                unprompted.add(ref)
        rows.append(dict(sid=sid, project=project, model=model or "?", calls=calls or 0, dur=dur or 0,
                         toks=toks or 0, reads=len(reads), exposed=len(exposed), acted=len(acted),
                         unprompted=len(unprompted),
                         closed=sum(1 for r in unprompted if states.get(int(r)) in ("completed", "cancelled")),
                         open_=sum(1 for r in unprompted if states.get(int(r)) not in (None, "completed", "cancelled")),
                         unknown=sum(1 for r in unprompted if states.get(int(r)) is None)))
    return rows


def pct(a, b):
    return f"{100 * a / b:.1f}%" if b else "n/a"


def strat_ratio(rows, key, boot=500):
    """Matched comparison: within (project, model) strata, mean log-ratio of detour vs non-detour sessions."""
    strata = defaultdict(lambda: ([], []))
    for r in rows:
        if r[key] > 0:
            strata[(r["project"], r["model"])][0 if r["unprompted"] else 1].append(r[key])
    def est(sample):
        diffs, w = [], []
        for (d, n) in sample.values():
            if d and n:
                diffs.append(math.log(statistics.mean(d)) - math.log(statistics.mean(n)))
                w.append(min(len(d), len(n)))
        return math.exp(sum(x * y for x, y in zip(diffs, w)) / sum(w)) - 1 if w else None
    point = est(strata)
    if point is None:
        return None
    rnd, bs = random.Random(0), []
    for _ in range(boot):
        s = {k: ([rnd.choice(d) for _ in d], [rnd.choice(n) for _ in n]) for k, (d, n) in strata.items() if d and n}
        e = est(s)
        if e is not None:
            bs.append(e)
    bs.sort()
    return point, (bs[int(.025 * len(bs))], bs[int(.975 * len(bs)) - 1]) if len(bs) > 20 else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--since"); ap.add_argument("--until"); ap.add_argument("--db", default=INSIGHTS)
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    c = ro(a.db)
    rows = analyse(c, sessions(c, a.since, a.until), item_states())
    with_reads = [r for r in rows if r["reads"]]
    exp = [r for r in with_reads if r["exposed"]]
    out: dict = dict(
        sessions=len(rows), sessions_with_reads=len(with_reads), exposed_sessions=len(exp),
        exposure_rate=pct(len(exp), len(with_reads)),
        refs_exposed=sum(r["exposed"] for r in exp), refs_acted=sum(r["acted"] for r in exp),
        action_rate=pct(sum(r["acted"] for r in exp), sum(r["exposed"] for r in exp)),
        unprompted_share=pct(sum(r["unprompted"] for r in exp), sum(r["acted"] for r in exp)),
        unprompted_detours=sum(r["unprompted"] for r in exp),
        detour_closed=sum(r["closed"] for r in exp), detour_open=sum(r["open_"] for r in exp),
        detour_state_unknown=sum(r["unknown"] for r in exp),
        detour_session_rate_per_exposed=pct(sum(1 for r in exp if r["unprompted"]), len(exp)),
    )
    for k, label in (("calls", "steps"), ("toks", "tokens"), ("dur", "wait")):
        res = strat_ratio(exp if len(exp) > 30 else with_reads, k)
        out[f"matched_overhead_{label}"] = None if res is None else {"effect": round(res[0], 3), "ci95": res[1] and [round(x, 3) for x in res[1]]}
    if a.json:
        print(json.dumps(out, indent=2)); return
    for k, v in out.items():
        print(f"{k:34} {v}")
    print("\nCaveats: detour/non-detour compared within (project, model) strata among exposed sessions; "
          "task difficulty is NOT controlled -- treat overhead as an upper bound.", file=sys.stderr)


if __name__ == "__main__":
    main()
