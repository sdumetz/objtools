"""Optional Linux/Unix stress check; uses ~1 GiB disk with the default 9M faces.

Build release first, then: python3 tools/stress_import.py
The source is larger than the child's 64 MiB address-space limit.
"""
import json
import pathlib
import resource
import subprocess
import tempfile
import time

triangles = 9_000_000
limit = 64 * 1024 * 1024
with tempfile.TemporaryDirectory(prefix="objtools-stress-") as directory:
    directory = pathlib.Path(directory)
    source = directory / "huge.obj"
    with source.open("wb") as stream:
        stream.write(b"o huge\nv 651000 6862000 120\nv 651001 6862000 120\nv 651000 6862001 120\n")
        block = b"f 1 2 3\n" * 65536
        left = triangles
        while left:
            count = min(left, 65536)
            stream.write(block[: count * 8])
            left -= count

    def cap_memory():
        resource.setrlimit(resource.RLIMIT_AS, (limit, limit))

    started = time.monotonic()
    subprocess.run(
        ["target/release/objtools", "import", str(source), "--center", "--progress", "--tmp-dir", str(directory)],
        check=True,
        preexec_fn=cap_memory,
    )
    model = json.loads(source.with_suffix(".gltf").read_text())
    primitives = model["meshes"][0]["primitives"]
    counts = [model["accessors"][p["indices"]]["count"] for p in primitives]
    assert sum(counts) == triangles * 3
    assert max(counts) <= 65536 * 3
    assert source.stat().st_size > limit
    print(json.dumps({"source_bytes": source.stat().st_size, "address_space_limit_bytes": limit,
                      "triangles": triangles, "primitives": len(primitives),
                      "elapsed_seconds": round(time.monotonic() - started, 2)}, indent=2))
