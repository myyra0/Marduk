#!/usr/bin/env python3
"""Thin supervisor around `opencode run` for unattended subagent-driven-development (stdlib only).

  supervisor.py doctor
  supervisor.py run PLAN_FILE [--max-stuck 5]
  supervisor.py status PLAN_FILE
  supervisor.py check-msg FILE          (used by scripts/hooks/commit-msg)

It does not plan, dispatch or review; your superpowers skill does that. It only keeps the controller session going:
re-runs it after provider limits and crashes (the skill resumes from its ledger), sleeps through limit windows, and notifies you.
Env (see harness.env): MODEL_SOL, MODEL_DEEPSEEK, MODEL_BEAM, LEGACY_PATH; optional NOTIFY_CMD, RETRY_SLEEP=1800, SESSION_TIMEOUT=14400.
"""
import argparse, os, re, shlex, shutil, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOGS = ROOT / ".logs"
MODEL_VARS = ("MODEL_SOL", "MODEL_DEEPSEEK", "MODEL_BEAM")
LIMIT_RE = re.compile(r"rate.?limit|usage limit|too many requests|quota (exceeded|exhausted)|\b429\b", re.I)
# Commit-message guard. The old name is split so a repo-wide grep for it stays clean.
DENY_RE = re.compile(r"ameth" r"yst|legacy|\bport(ed|ing)\b|\bports? (from|of|over)\b|(^|:\s*)port\b|rewrit(e|ten|ing)|\bparity\b",
                     re.I | re.M)


def load_env():
    """Read ROOT/harness.env so `supervisor.py <cmd>` needs no shell setup; real env wins."""
    path = ROOT / "harness.env"
    if not path.exists():
        return
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        os.environ.setdefault(key.strip(), value.strip().strip("\"'"))


def sh(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, **kw)


def git(*args):
    return sh(["git", *args])


def notify(msg):
    print(f"[supervisor] {msg}", flush=True)
    if os.environ.get("NOTIFY_CMD"):
        subprocess.run(shlex.split(os.environ["NOTIFY_CMD"]) + [msg], capture_output=True)
    elif shutil.which("notify-send"):
        subprocess.run(["notify-send", "Marduk build", msg], capture_output=True)


def limited(code, out):
    """A session that died on a provider limit exits non-zero or prints almost nothing."""
    return (code != 0 or len(out) < 1500) and bool(LIMIT_RE.search(out[-600:]))


def ledger(plan):
    return ROOT / ".superpowers" / "sdd" / Path(plan).stem / "progress.md"


def progress(plan):
    p = ledger(plan)
    return len(re.findall(r"^Task \d+: complete", p.read_text(), re.M)) if p.exists() else 0


def doctor(strict=False):
    problems = []
    for p in (ROOT / "AGENTS.md", ROOT / "opencode.jsonc"):
        if p.exists() and "FILL_ME" in p.read_text():
            problems.append(f"replace FILL_ME in {p.name}")
    for v in (*MODEL_VARS, "LEGACY_PATH"):
        if not os.environ.get(v):
            problems.append(f"env var {v} not set (source harness.env)")
    lp = os.environ.get("LEGACY_PATH")
    if lp and not Path(lp).is_dir():
        problems.append(f"LEGACY_PATH is not a directory: {lp}")
    if git("rev-parse", "--git-dir").returncode != 0:
        problems.append("not a git repository")
    elif git("config", "core.hooksPath").stdout.strip() != "scripts/hooks":
        problems.append("run: git config core.hooksPath scripts/hooks")
    if not shutil.which("opencode"):
        problems.append("opencode not on PATH")
    else:
        try:
            r = subprocess.run(["opencode", "models"], capture_output=True, text=True, timeout=60)
            known = {l.strip() for l in r.stdout.splitlines() if "/" in l}
            wanted = {os.environ[v] for v in MODEL_VARS if os.environ.get(v)}
            if known:
                problems += [f"model not offered by opencode: {m}" for m in sorted(wanted - known)]
        except Exception as e:
            print(f"(could not list models: {e})")
    for p in problems:
        print("-", p)
    if problems and strict:
        sys.exit(1)
    print("doctor: ok" if not problems else f"doctor: {len(problems)} problem(s)")


def check_msg(path):
    text = "\n".join(l for l in Path(path).read_text().splitlines() if not l.startswith("#"))
    m = DENY_RE.search(text)
    if m:
        sys.exit(f"commit rejected: message mentions '{m.group(0)}'. Describe the capability added; "
                 "don't reference porting, the legacy app, or its name.")


def prompt(plan, branch):
    return (
        f"Use the subagent-driven-development skill to execute the plan at {plan}. You are the controller. "
        f"Work happens on branch {branch} in this workspace: treat worktree setup as done and consent to work on this branch as given. "
        "If the plan has a ledger, resume from it and trust the ledger and git log over your own recollection. "
        "Run unattended: do not ask questions; rule on ambiguities and record the rulings as the skill says. "
        "Follow AGENTS.md for which agent (subagent_type) fills each role. "
        "When the final whole-branch review is clean, do not run finishing-a-development-branch; list your rulings in your last "
        "message and end with a line containing only SDD_DONE. If one of the skill's four stop conditions forces you to stop, "
        "end with a line `SDD_STOPPED: <reason>`."
    )


def status(plan):
    p = ledger(plan)
    print(f"{progress(plan)} task(s) complete")
    if p.exists():
        print("\n".join(p.read_text().splitlines()[-12:]))


def run(plan, max_stuck):
    doctor(strict=True)
    if not (ROOT / plan).exists():
        sys.exit(f"plan not found: {plan}")
    branch = git("branch", "--show-current").stdout.strip()
    if branch in ("", "main", "master"):
        branch = f"build/{Path(plan).stem}"
        if git("switch", branch).returncode != 0:
            git("switch", "-c", branch)
    LOGS.mkdir(exist_ok=True)
    sleep_s = int(os.environ.get("RETRY_SLEEP", 1800))
    stuck, n = 0, 0
    while True:
        n += 1
        before = progress(plan)
        try:
            r = subprocess.run(["opencode", "run", "--agent", "orchestrator", prompt(plan, branch)], cwd=ROOT,
                               stdin=subprocess.DEVNULL, capture_output=True, text=True,
                               timeout=int(os.environ.get("SESSION_TIMEOUT", 14400)))
            code, out = r.returncode, (r.stdout or "") + (r.stderr or "")
        except subprocess.TimeoutExpired:
            code, out = 124, "[supervisor] session timeout"
        (LOGS / f"session-{n:03d}.log").write_text(out)
        tail = out[-1500:]
        if re.search(r"^SDD_DONE\s*$", tail, re.M):
            notify(f"Plan complete ({progress(plan)} tasks). Review the branch {branch}; the final message lists the rulings.")
            return
        m = re.search(r"^SDD_STOPPED:\s*(.*)$", tail, re.M)
        if m:
            notify(f"Stopped, needs you: {m.group(1).strip()[:200]}")
            sys.exit(2)
        made_progress = progress(plan) > before
        if made_progress:
            stuck = 0
        if limited(code, out):
            notify(f"Provider limit hit ({progress(plan)} tasks done). Waiting {sleep_s // 60} min, then resuming.")
            time.sleep(sleep_s)
            continue
        if made_progress:
            notify(f"Session ended after progress ({progress(plan)} tasks done); resuming.")
            continue
        stuck += 1
        if stuck >= max_stuck:
            notify(f"Stuck: {max_stuck} sessions in a row ended without finishing, progress or a limit. See .logs/session-{n:03d}.log")
            sys.exit(1)
        notify(f"Session ended early (exit {code}); resuming ({stuck}/{max_stuck}).")


def main():
    load_env()
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("doctor")
    r = sub.add_parser("run"); r.add_argument("plan"); r.add_argument("--max-stuck", type=int, default=5)
    s = sub.add_parser("status"); s.add_argument("plan")
    c = sub.add_parser("check-msg"); c.add_argument("file")
    a = ap.parse_args()
    {"doctor": lambda: doctor(), "run": lambda: run(a.plan, a.max_stuck), "status": lambda: status(a.plan),
     "check-msg": lambda: check_msg(a.file)}[a.cmd]()


if __name__ == "__main__":
    main()
