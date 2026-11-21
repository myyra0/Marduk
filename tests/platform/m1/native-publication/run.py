"""Link the real storage witness without a reverse platform dependency."""
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: run.py EXISTING_DISPOSABLE_SCRATCH_PARENT")
    scratch = Path(sys.argv[1]).resolve(strict=True)
    print("R03 environment", platform.uname(), scratch, flush=True)
    run("git", "rev-parse", "HEAD")
    run("rustc", "-Vv")
    if sys.platform.startswith("linux"):
        run("id")
        run("findmnt", "-T", str(scratch), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS")
    elif sys.platform == "win32":
        run("whoami", "/all")
    else:
        raise SystemExit("R03 NOT RUN: unsupported host")
    build = run("cargo", "build", "--locked", "-p", "marduk-storage",
                "--message-format=json", stdout=subprocess.PIPE, text=True)
    names = {"marduk_core", "marduk_storage"}
    libs = {}
    dirs = set()
    for line in build.stdout.splitlines():
        obj = json.loads(line)
        if obj["reason"] == "compiler-artifact":
            for name in obj["filenames"]:
                if name.endswith(".rlib"):
                    artifact = Path(name)
                    dirs.add(artifact.parent)
                    if obj["target"]["name"] in names:
                        libs[obj["target"]["name"]] = artifact
    if libs.keys() != names:
        raise SystemExit("expected the two Cargo build artifacts")
    with tempfile.TemporaryDirectory(prefix="marduk-r03-probe-") as out:
        probe = Path(out) / ("catalog.exe" if sys.platform == "win32" else "catalog")
        extern = [part for k, v in sorted(libs.items()) for part in ("--extern", f"{k}={v}")]
        search = [part for path in sorted(dirs) for part in ("-L", f"dependency={path}")]
        run("rustc", "--edition=2024", str(HERE / "library.rs"), *search,
            *extern, "-o", str(probe))
        env = dict(os.environ, MARDUK_R03_SCRATCH=str(scratch), MARDUK_R03_LIBRARY_PROBE=str(probe))
        run("cargo", "test", "--locked", "-p", "marduk-platform", "--test", "native_publication",
            "--", "--nocapture", "--test-threads=1", env=env)


if __name__ == "__main__":
    main()
