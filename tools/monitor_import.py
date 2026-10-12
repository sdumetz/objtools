"""Run the release importer, recording Linux process RSS and progress once per second.

python3 tools/monitor_import.py input.obj output.gltf [import options ...]
Reports live in the destination folder. RSS includes the memory-mapped spools, which are
evictable page cache; the anonymous part (RssAnon) is the importer's own heap.
"""
import csv
import json
import pathlib
import resource
import subprocess
import sys
import time

source, output, *options = sys.argv[1:]
output = pathlib.Path(output)
output.parent.mkdir(parents=True, exist_ok=True)
log_path = output.with_suffix(".import.log")
csv_path = output.with_suffix(".memory.csv")
report_path = output.with_suffix(".import.json")
if any(p.exists() for p in (log_path, csv_path, report_path)):
    prefix = output.with_name(output.stem + f"-retry-{time.time_ns()}")
    log_path = pathlib.Path(str(prefix) + ".import.log")
    csv_path = pathlib.Path(str(prefix) + ".memory.csv")
    report_path = pathlib.Path(str(prefix) + ".import.json")
for candidate in (output, output.with_suffix(".bin"), log_path, csv_path, report_path):
    if candidate.exists():
        raise SystemExit(f"Refusing to overwrite {candidate}")
started = time.monotonic()
peak_rss = 0
peak_anon = 0
with log_path.open("x") as log, csv_path.open("x", newline="") as samples:
    writer = csv.writer(samples)
    writer.writerow(["elapsed_seconds", "rss_kib", "peak_rss_kib", "virtual_kib", "anon_kib"])
    command = [str(pathlib.Path("target/release/objtools").resolve()), "import", source, "-o", str(output), *options]
    process = subprocess.Popen(command, stderr=log, stdout=log)
    last_notice = -10
    while process.poll() is None:
        try:
            values = {}
            for line in pathlib.Path(f"/proc/{process.pid}/status").read_text().splitlines():
                key, _, rest = line.partition(":")
                if key in ("VmRSS", "VmHWM", "VmSize", "RssAnon"):
                    values[key] = int(rest.split()[0])
            peak_rss = max(peak_rss, values.get("VmHWM", 0))
            peak_anon = max(peak_anon, values.get("RssAnon", 0))
            elapsed = time.monotonic() - started
            writer.writerow([round(elapsed, 2), values.get("VmRSS", 0), peak_rss, values.get("VmSize", 0), values.get("RssAnon", 0)])
            samples.flush()
            if elapsed - last_notice >= 10:
                lines = log_path.read_text().splitlines()
                print(f"{elapsed:.0f}s: RSS {values.get('VmRSS', 0)/1024:.1f} MiB (heap {values.get('RssAnon', 0)/1024:.1f} MiB), peak {peak_rss/1024:.1f} MiB; {lines[-1] if lines else 'starting'}", flush=True)
                last_notice = elapsed
        except FileNotFoundError:
            pass
        time.sleep(1)
    code = process.wait()
peak_rss = max(peak_rss, resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
report = {"command": command, "exit_code": code, "elapsed_seconds": round(time.monotonic()-started, 2),
          "peak_rss_kib": peak_rss, "peak_anon_kib": peak_anon, "log": str(log_path), "memory_samples": str(csv_path), "report": str(report_path), "source_bytes": pathlib.Path(source).stat().st_size,
          "output_files": {p.name: p.stat().st_size for p in (output, output.with_suffix('.bin')) if p.exists()}}
report_path.write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report, indent=2), flush=True)
raise SystemExit(code)
