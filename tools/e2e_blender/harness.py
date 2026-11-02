#!/usr/bin/env python3
"""End-to-end check of `objtools import`: does a model look the same in Blender afterwards?

For each Khronos sample asset, renders the source and the imported output in Blender from the same
cameras, compares the pixels and what Blender loaded, and writes out/e2e/report.html. Results are
saved as they come, so an interrupted run resumes where it stopped.

    python3 tools/e2e_blender/harness.py --models DamagedHelmet,Duck
    python3 tools/e2e_blender/harness.py --max-mb 100            # everything small enough
    python3 tools/e2e_blender/harness.py --report                # rewrite the report only
    python3 tools/e2e_blender/harness.py --negative DamagedHelmet # prove a breakage is flagged

Needs a release build, Blender on PATH and Pillow. Downloads and renders stay in out/e2e, which is
ignored by git. Validator results appear if NODE_PATH reaches gltf-validator.

MeshoptCubeTest shows as DIFFERENT on Blender 5.2: its importer decodes meshopt's COLOR filter with
the exponential one (gltf2_io_binary_meshopt.py), which miscolours six cubes of the source only.
"""
import argparse
import html
import json
import math
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request

from PIL import Image, ImageChops, ImageStat

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
WORK = REPO / "out" / "e2e"
OBJTOOLS = REPO / "target" / "release" / "objtools"
RAW = "https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models"
TREE = "https://api.github.com/repos/KhronosGroup/glTF-Sample-Assets/git/trees/main?recursive=1"
VIEWS = ("front", "back", "top")
# A pixel counts as different past this, out of 255, which leaves float32 rounding alone.
PIXEL_THRESHOLD = 16
# A view fails when more than this share of the model's pixels differ.
VIEW_FAILS_AT = 0.1  # percent
# Bounds may drift this far, relative to the model's size, before they are reported.
BOUNDS_TOLERANCE = 1e-5
# Each mode: import options, and whether extras.objtools must bring the model back.
MODES = {
    "plain": {"args": [], "undo": False},
    "center+z": {"args": ["--center", "--up-axis", "z"], "undo": True},
    "reimport": {"args": ["--center"], "undo": True, "from": "center+z"},
}
# Which variants of a model to try, the first being the one all modes run on.
VARIANTS = ("glTF", "glTF-Binary", "glTF-Embedded", "glTF-Quantized", "glTF-WEBP", "glTF-KTX-BasisU",
            "glTF-JPG", "glTF-Draco", "glTF-Meshopt", "glTF-Meshopt-EXT")


def fetch(url, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    partial = path.with_name(path.name + ".part")
    with urllib.request.urlopen(url, timeout=300) as response, open(partial, "wb") as out:
        shutil.copyfileobj(response, out)
    partial.rename(path)


def catalogue():
    """The models, and each variant's files with their sizes, from the repository's own index."""
    index = WORK / "model-index.json"
    tree = WORK / "tree.json"
    if not index.exists():
        fetch(f"{RAW}/model-index.json", index)
    if not tree.exists():
        fetch(TREE, tree)
    files = {}
    for entry in json.loads(tree.read_text())["tree"]:
        parts = entry["path"].split("/")
        if entry["type"] == "blob" and len(parts) >= 4 and parts[0] == "Models":
            files.setdefault((parts[1], parts[2]), []).append(("/".join(parts[3:]), entry["size"]))
    return json.loads(index.read_text()), files


def download(model, variant, files):
    target = WORK / "assets" / model / variant
    for name, size in files[(model, variant)]:
        path = target / name
        if not path.exists() or path.stat().st_size != size:
            fetch(f"{RAW}/{urllib.parse.quote(model)}/{urllib.parse.quote(variant)}/{urllib.parse.quote(name)}", path)
    return target


def run(command, timeout=1800):
    try:
        done = subprocess.run(command, capture_output=True, text=True, errors="replace", timeout=timeout)
        return done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired:
        return -1, "", f"timed out after {timeout} s"


def blender(source, out, engine, size, samples, camera=None, undo=False):
    """Renders unless already done, and returns what Blender recorded."""
    record = out / "scene.json"
    if not record.exists():
        command = ["blender", "-b", "--factory-startup", "--disable-autoexec", "--python", str(HERE / "render.py"),
                   "--", "--input", str(source), "--out", str(out), "--engine", engine, "--size", str(size),
                   "--samples", str(samples)]
        if camera:
            command += ["--camera", str(camera)]
        if undo:
            command.append("--undo")
        out.mkdir(parents=True, exist_ok=True)
        code, stdout, stderr = run(command)
        (out / "blender.log").write_text(stdout[-20000:] + stderr[-5000:])
        if not record.exists():
            return {"ok": False, "error": f"Blender exited with {code}"}
    return json.loads(record.read_text())


def compare_view(a_path, b_path, diff_path):
    """How much of the model's pixels changed. Only pixels either render covers count."""
    a, b = Image.open(a_path).convert("RGBA"), Image.open(b_path).convert("RGBA")
    if a.size != b.size:
        return {"error": "sizes differ"}
    alpha_a, alpha_b = a.getchannel("A"), b.getchannel("A")
    mask = ImageChops.lighter(alpha_a, alpha_b).point(lambda v: 255 if v > 8 else 0)
    covered = ImageStat.Stat(mask).sum[0] / 255
    bands = ImageChops.difference(a, b).split()
    worst = bands[0]
    for band in bands[1:]:
        worst = ImageChops.lighter(worst, band)
    histogram = worst.histogram(mask=mask)
    total = sum(histogram) or 1
    over = sum(histogram[PIXEL_THRESHOLD + 1:])
    silhouette = ImageChops.difference(alpha_a, alpha_b).point(lambda v: 255 if v > 32 else 0)
    # Amplified, so a faint difference can be seen in the report.
    worst.point(lambda v: min(255, v * 8)).convert("RGB").save(diff_path)
    return {
        "covered": int(covered),
        "percent": round(100 * over / total, 3),
        "mean": round(sum(i * c for i, c in enumerate(histogram)) / total, 3),
        "max": max((i for i, c in enumerate(histogram) if c), default=0),
        "silhouette": int(ImageStat.Stat(silhouette).sum[0] / 255),
    }


def compare_scene(source, output):
    """Differences in what Blender loaded, which explain a pixel difference or reveal a hidden one."""
    notes = {}
    a, b = source["scene"], output["scene"]
    for key in ("triangles", "vertices", "materials", "actions"):
        if a[key] != b[key]:
            notes[key] = [a[key], b[key]]
    for kind in ("MESH", "LIGHT", "ARMATURE"):
        if a["objects"][kind] != b["objects"][kind]:
            notes[kind.lower() + "s"] = [a["objects"][kind], b["objects"][kind]]
    if sorted(map(tuple, a["images"])) != sorted(map(tuple, b["images"])):
        notes["images"] = [len(a["images"]), len(b["images"])]
    if a["bounds"] and b["bounds"]:
        size = max(math.dist(a["bounds"][0], a["bounds"][1]), 1e-9)
        drift = max(abs(x - y) for p, q in zip(a["bounds"], b["bounds"]) for x, y in zip(p, q))
        if drift / size > BOUNDS_TOLERANCE:
            notes["bounds_drift"] = round(drift / size, 6)
    elif a["bounds"] != b["bounds"]:
        notes["bounds"] = "one is empty"
    return notes


def validate(path):
    """The Khronos validator's error codes, or None when it isn't installed."""
    code, stdout, stderr = run(["node", str(REPO / "tools" / "validate_gltf.cjs"), str(path)], 900)
    if "Cannot find module" in stderr:
        return None
    try:
        issues = json.loads(stdout)["issues"]
    except (ValueError, KeyError):
        return {"failed": stderr[-300:]}
    return sorted({m["code"] for m in issues["messages"] if m["severity"] == 0})


def main_file(directory):
    for pattern in ("*.glb", "*.gltf"):
        found = sorted(directory.glob(pattern))
        if found:
            return found[0]
    return None


def verdict(result):
    if result.get("refused"):
        return "refused"
    if result.get("import_error"):
        return "IMPORT-FAILED"
    if result.get("source_error"):
        return "source-unloadable"
    if result.get("render_error"):
        return "OUTPUT-UNLOADABLE"
    views = result.get("views", {}).values()
    # Nothing in view proves nothing, so it can't count as the same.
    if views and all(v.get("covered", 0) == 0 for v in views):
        return "note"
    if any(v.get("percent", 100) > VIEW_FAILS_AT for v in views):
        return "noise" if result.get("noise") else "DIFFERENT"
    if result.get("new_validator_errors") or set(result.get("scene_notes", {})) - {"materials"}:
        return "note"
    return "same"


def check(model, variant, mode, source_file, source_out, args):
    """Imports in one mode, renders the output and compares it with the source's renders."""
    settings = MODES[mode]
    base = WORK / "runs" / model / variant
    out = base / mode
    result = {"model": model, "variant": variant, "mode": mode}
    source = blender(source_file, source_out, args.engine, args.size, args.samples)
    if not source.get("ok"):
        result["source_error"] = source.get("error", "")[-400:]
        return result
    input_file = source_file
    if "from" in settings:
        input_file = base / settings["from"] / "out.gltf"
        if not input_file.exists():
            result["refused"] = f"{settings['from']} produced no output"
            return result
    output = out / "out.gltf"
    if not output.exists():
        out.mkdir(parents=True, exist_ok=True)
        code, stdout, stderr = run([str(OBJTOOLS), "import", str(input_file), "-o", str(output), *settings["args"]])
        (out / "import.log").write_text(stdout + stderr)
        if code != 0:
            needs = ("not supported", "can't be converted", "no uncompressed data")
            key = "refused" if any(n in stderr for n in needs) else "import_error"
            result[key] = stderr.strip()[-400:]
            return result
    result["warnings"] = [l for l in (out / "import.log").read_text().splitlines() if l.startswith("Warning")]
    rendered = blender(output, out / "render", args.engine, args.size, args.samples,
                       camera=source_out / "camera.json", undo=settings["undo"])
    if not rendered.get("ok"):
        result["render_error"] = rendered.get("error", "")[-400:]
        return result
    result["scene_notes"] = compare_scene(source, rendered)
    result["views"] = {v: compare_view(source_out / f"{v}.png", out / "render" / f"{v}.png", out / f"diff-{v}.png")
                       for v in VIEWS}
    # Worth a second look only when something differs: is the source even stable against itself?
    if any(v.get("percent", 0) > VIEW_FAILS_AT for v in result["views"].values()):
        control = source_out.parent / "source-control"
        blender(source_file, control, args.engine, args.size, args.samples, camera=source_out / "camera.json")
        if (control / "front.png").exists():
            floor = max(compare_view(source_out / f"{v}.png", control / f"{v}.png", control / f"diff-{v}.png")
                        .get("percent", 0) for v in VIEWS)
            result["noise_floor"] = floor
            worst = max(v.get("percent", 0) for v in result["views"].values())
            result["noise"] = worst <= floor * 1.5 + VIEW_FAILS_AT
    errors = validate(output)
    if errors is not None:
        before = validate(source_file)
        if isinstance(errors, list) and isinstance(before, list):
            result["new_validator_errors"] = sorted(set(errors) - set(before))
        else:
            result["validator"] = [errors, before]
    return result


def results_path():
    return WORK / "results.jsonl"


def load_results():
    path = results_path()
    out = {}
    if path.exists():
        for line in path.read_text().splitlines():
            r = json.loads(line)
            out[(r["model"], r["variant"], r["mode"])] = r
    return out


def run_all(args):
    index, files = catalogue()
    wanted = set(args.models.split(",")) if args.models else None
    done = load_results()
    jobs = []
    for entry in index:
        model = entry["name"]
        if wanted and model not in wanted:
            continue
        if args.tag and args.tag not in entry.get("tags", []):
            continue
        for number, variant in enumerate(v for v in VARIANTS if (model, v) in files):
            size = sum(s for _, s in files[(model, variant)]) / 1e6
            if size > args.max_mb:
                continue
            for mode in MODES if number == 0 else ("plain",):
                if args.only_base and number:
                    continue
                if (model, variant, mode) not in done:
                    jobs.append((model, variant, mode))
    print(f"{len(jobs)} checks to run, {len(done)} already done", flush=True)
    started = time.time()
    for position, (model, variant, mode) in enumerate(jobs):
        t = time.time()
        try:
            directory = download(model, variant, files)
            source_file = main_file(directory)
            if source_file is None:
                continue
            source_out = WORK / "runs" / model / variant / "source"
            result = check(model, variant, mode, source_file, source_out, args)
        except Exception as e:  # One asset's failure shouldn't end a long run.
            result = {"model": model, "variant": variant, "mode": mode, "harness_error": repr(e)[:400], "import_error": "harness: " + repr(e)[:300]}
        result["verdict"] = verdict(result)
        result["seconds"] = round(time.time() - t, 1)
        with results_path().open("a") as f:
            f.write(json.dumps(result) + "\n")
        remaining = (time.time() - started) / (position + 1) * (len(jobs) - position - 1)
        print(f"[{position + 1}/{len(jobs)}] {model} {variant} {mode}: {result['verdict']} "
              f"({result['seconds']} s, ~{remaining / 60:.0f} min left)", flush=True)
    write_report()


ORDER = ["IMPORT-FAILED", "OUTPUT-UNLOADABLE", "DIFFERENT", "note", "noise", "source-unloadable", "refused", "same"]


def worst_percent(r):
    return max((v.get("percent", 0) for v in r.get("views", {}).values()), default=0)


def write_report():
    results = list(load_results().values())
    results.sort(key=lambda r: (ORDER.index(r["verdict"]), -worst_percent(r), r["model"]))
    counts = {k: sum(r["verdict"] == k for r in results) for k in ORDER}
    root = WORK
    rows = []
    for r in results:
        details = []
        if r.get("views"):
            details.append(" ".join(f"{k} {v.get('percent')}% (mean {v.get('mean')}, silhouette {v.get('silhouette')})"
                                    for k, v in r["views"].items()))
        for key in ("scene_notes", "new_validator_errors", "warnings", "noise_floor", "refused", "import_error",
                    "render_error", "source_error"):
            if r.get(key):
                details.append(f"{key}: {r[key]}")
        images = ""
        if r["verdict"] in ("DIFFERENT", "note", "noise") and r.get("views"):
            run_dir = pathlib.Path("runs") / r["model"] / r["variant"]
            view = max(r["views"], key=lambda k: r["views"][k].get("percent", 0))
            images = "".join(f'<a href="{p}"><img src="{p}" width=200></a>' for p in
                             (run_dir / "source" / f"{view}.png", run_dir / r["mode"] / "render" / f"{view}.png",
                              run_dir / r["mode"] / f"diff-{view}.png"))
        rows.append(f"<tr class={html.escape(r['verdict'])}><td>{html.escape(r['verdict'])}</td>"
                    f"<td>{html.escape(r['model'])}</td><td>{html.escape(r['variant'])}</td><td>{html.escape(r['mode'])}</td>"
                    f"<td>{images}<pre>{html.escape(chr(10).join(details))}</pre></td></tr>")
    summary = ", ".join(f"{k}: {v}" for k, v in counts.items() if v)
    (root / "report.html").write_text(
        "<!doctype html><meta charset=utf-8><title>objtools e2e</title><style>"
        "body{font:14px sans-serif;margin:16px}table{border-collapse:collapse}td{border:1px solid #bbb;padding:4px;"
        "vertical-align:top}pre{white-space:pre-wrap;margin:4px 0;max-width:800px}.same td{color:#777}"
        ".DIFFERENT td:first-child,.IMPORT-FAILED td:first-child,.OUTPUT-UNLOADABLE td:first-child{background:#fbb}"
        ".note td:first-child{background:#fe9}</style>"
        f"<h1>objtools end-to-end</h1><p>{html.escape(summary)}</p><table>{''.join(rows)}</table>")
    print(summary)
    print(f"Report: {root / 'report.html'}")


def negative(args):
    """Breaks an output in known ways. A harness that misses one of these can't be trusted."""
    index, files = catalogue()
    breakages = {
        "moved 1%": move, "no base colour textures": no_textures, "flipped V": flip_v,
        "lost a primitive": lost_primitive, "roughness changed": roughness, "wrong origin": wrong_origin,
    }
    missed = 0
    for model in args.negative.split(","):
        variant = "glTF"
        directory = download(model, variant, files)
        source_file = main_file(directory)
        source_out = WORK / "runs" / model / variant / "source"
        source = blender(source_file, source_out, args.engine, args.size, args.samples)
        clean = WORK / "negative" / model / "clean"
        if not (clean / "out.gltf").exists():
            clean.mkdir(parents=True, exist_ok=True)
            run([str(OBJTOOLS), "import", str(source_file), "-o", str(clean / "out.gltf"), "--center", "--up-axis", "z"])
        for name, breakage in breakages.items():
            out = WORK / "negative" / model / name.replace(" ", "-").replace("%", "pct")
            if out.exists():
                shutil.rmtree(out)
            shutil.copytree(clean, out)
            root = json.loads((out / "out.gltf").read_text())
            breakage(root, source)
            (out / "out.gltf").write_text(json.dumps(root))
            rendered = blender(out / "out.gltf", out / "render", args.engine, args.size, args.samples,
                               camera=source_out / "camera.json", undo=True)
            views = {v: compare_view(source_out / f"{v}.png", out / "render" / f"{v}.png", out / f"diff-{v}.png") for v in VIEWS}
            worst = max(v["percent"] for v in views.values())
            notes = compare_scene(source, rendered) if rendered.get("ok") else {"render": "failed"}
            caught = worst > VIEW_FAILS_AT or bool(set(notes) - {"materials"})
            missed += not caught
            print(f"{model:24s} {name:26s} worst view {worst:7.3f}%  notes {notes}  {'caught' if caught else 'MISSED'}", flush=True)
    print("All caught" if not missed else f"{missed} MISSED")
    return missed


def move(root, source):
    low, high = source["scene"]["bounds"]
    amount = math.dist(low, high) * 0.01
    for node in root["nodes"]:
        if node.get("name") == "objtools":
            t = node.get("translation", [0, 0, 0])
            node["translation"] = [t[0] + amount, t[1], t[2]]


def no_textures(root, _):
    for material in root.get("materials", []):
        material.get("pbrMetallicRoughness", {}).pop("baseColorTexture", None)


def flip_v(root, _):
    root.setdefault("extensionsUsed", []).append("KHR_texture_transform")
    for material in root.get("materials", []):
        slot = material.get("pbrMetallicRoughness", {}).get("baseColorTexture")
        if slot:
            slot.setdefault("extensions", {})["KHR_texture_transform"] = {"scale": [1, -1], "offset": [0, 1]}


def lost_primitive(root, _):
    for mesh in sorted(root["meshes"], key=lambda m: -len(m["primitives"]))[:1]:
        if len(mesh["primitives"]) > 1:
            mesh["primitives"].pop()
        else:
            mesh["primitives"][0]["indices_count_hint"] = 0
            accessor = root["accessors"][mesh["primitives"][0]["indices"]]
            accessor["count"] = max(3, accessor["count"] // 2 // 3 * 3)


def roughness(root, _):
    for material in root.get("materials", []):
        pbr = material.setdefault("pbrMetallicRoughness", {})
        pbr["roughnessFactor"] = 0.05
        pbr["metallicFactor"] = 1
        pbr.pop("metallicRoughnessTexture", None)


def wrong_origin(root, source):
    low, high = source["scene"]["bounds"]
    amount = math.dist(low, high) * 0.01
    origin = root["extras"]["objtools"]["origin"]
    origin[0] = str(float(origin[0]) + amount)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--models", help="comma-separated names; default all")
    parser.add_argument("--tag", help="only models with this tag: core, extension, testing, showcase, ...")
    parser.add_argument("--max-mb", type=float, default=1e9, help="skip variants larger than this")
    parser.add_argument("--only-base", action="store_true", help="only each model's first variant")
    parser.add_argument("--engine", default="BLENDER_EEVEE")
    parser.add_argument("--size", type=int, default=512)
    parser.add_argument("--samples", type=int, default=16)
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--negative", help="comma-separated names to break on purpose")
    parser.add_argument("--retry", help="forget results with this verdict, to run them again")
    args = parser.parse_args()
    if not OBJTOOLS.exists():
        sys.exit(f"Build first: cargo build --release ({OBJTOOLS} is missing)")
    WORK.mkdir(parents=True, exist_ok=True)
    if args.report:
        write_report()
    elif args.negative:
        sys.exit(1 if negative(args) else 0)
    else:
        if args.retry:
            kept = [r for r in load_results().values() if r["verdict"] != args.retry]
            results_path().write_text("".join(json.dumps(r) + "\n" for r in kept))
        run_all(args)


if __name__ == "__main__":
    main()
