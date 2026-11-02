"""Runs inside Blender: imports one glTF, renders it from fixed views and records what it loaded.

blender -b --factory-startup --disable-autoexec --python render.py -- --input FILE --out DIR
        [--camera FILE] [--undo] [--engine ENGINE] [--size N] [--samples N]

Without --camera, this is a source: the views are framed on its bounds and saved to DIR/camera.json.
With it, those exact views are reused, so a model that moved shows up in the pixels.

--undo moves the model back using only what extras.objtools records, by the documented formula:
undo the rotation, then add the origin. A wrong record therefore looks like wrong geometry.
"""
import argparse
import json
import math
import os
import sys
import time
import traceback

import bpy
from mathutils import Matrix, Vector

VIEWS = {"front": (35, 25), "back": (215, -20), "top": (120, 70)}


def parse():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--camera")
    parser.add_argument("--undo", action="store_true")
    parser.add_argument("--engine", default="BLENDER_EEVEE")
    parser.add_argument("--size", type=int, default=512)
    parser.add_argument("--samples", type=int, default=32)
    return parser.parse_args(sys.argv[sys.argv.index("--") + 1 :])


# glTF (x, y, z) is Blender (x, -z, y), which is how the importer converts it.
TO_BLENDER = Matrix(((1, 0, 0, 0), (0, 0, -1, 0), (0, 1, 0, 0), (0, 0, 0, 1)))


def recovery_matrix(path):
    """The documented way back to source coordinates, in Blender's frame."""
    record = gltf_json(path)["extras"]["objtools"]
    origin = [float(c) for c in record["origin"]]
    # `--up-axis z` applied (x, y, z) -> (x, z, -y); its inverse is (x, -z, y).
    if record["sourceUpAxis"] == "z":
        rotation = Matrix(((1, 0, 0, 0), (0, 0, -1, 0), (0, 1, 0, 0), (0, 0, 0, 1)))
    else:
        rotation = Matrix.Identity(4)
    # A rotated file's origin is in the source frame: rotate back first, then translate there.
    return TO_BLENDER @ Matrix.Translation(origin) @ rotation @ TO_BLENDER.inverted()


def gltf_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def world_points(obj, depsgraph):
    evaluated = obj.evaluated_get(depsgraph)
    mesh = evaluated.to_mesh()
    try:
        matrix = evaluated.matrix_world
        for vertex in mesh.vertices:
            yield matrix @ vertex.co
    finally:
        evaluated.to_mesh_clear()


def measure(scene):
    """What Blender loaded, which explains a pixel difference when one appears."""
    depsgraph = bpy.context.evaluated_depsgraph_get()
    low, high = [math.inf] * 3, [-math.inf] * 3
    triangles = vertices = 0
    # The importer adds hidden helpers, such as the shape it draws bones with: not part of the model.
    meshes = [o for o in scene.objects if o.type == "MESH" and not o.hide_render and o.visible_get()]
    for obj in meshes:
        evaluated = obj.evaluated_get(depsgraph)
        mesh = evaluated.to_mesh()
        mesh.calc_loop_triangles()
        triangles += len(mesh.loop_triangles)
        vertices += len(mesh.vertices)
        matrix = evaluated.matrix_world
        for vertex in mesh.vertices:
            p = matrix @ vertex.co
            for i in range(3):
                low[i] = min(low[i], p[i])
                high[i] = max(high[i], p[i])
        evaluated.to_mesh_clear()
    used = {s.material for o in meshes for s in o.material_slots if s.material}
    images = sorted(
        (i.size[0], i.size[1])
        for i in bpy.data.images
        if i.name not in ("Render Result", "Viewer Node") and i.type == "IMAGE"
    )
    return {
        "objects": {
            "MESH": len(meshes),
            **{t: sum(o.type == t for o in scene.objects) for t in ("LIGHT", "CAMERA", "ARMATURE")},
        },
        "triangles": triangles,
        "vertices": vertices,
        "materials": len(used),
        "images": images,
        "actions": len(bpy.data.actions),
        "bounds": None if not meshes or low[0] == math.inf else [low, high],
    }


def world_light(scene):
    """A built-in HDRI, so metals and glass have something to reflect, which plain grey wouldn't."""
    world = bpy.data.worlds.new("light")
    world.use_nodes = True
    nodes, links = world.node_tree.nodes, world.node_tree.links
    background = nodes["Background"]
    studio = os.path.join(bpy.utils.resource_path("LOCAL"), "datafiles", "studiolights", "world")
    candidates = sorted(f for f in os.listdir(studio) if f.endswith(".exr"))
    choice = "forest.exr" if "forest.exr" in candidates else candidates[0]
    image = nodes.new("ShaderNodeTexEnvironment")
    image.image = bpy.data.images.load(os.path.join(studio, choice))
    links.new(image.outputs["Color"], background.inputs["Color"])
    scene.world = world


def configure(scene, args):
    render = scene.render
    render.engine = args.engine
    render.resolution_x = render.resolution_y = args.size
    render.image_settings.file_format = "PNG"
    # The HDRI lights and reflects, but only the model's own pixels are compared.
    render.film_transparent = True
    render.image_settings.color_mode = "RGBA"
    scene.view_settings.view_transform = "Standard"
    scene.view_settings.look = "None"
    if args.engine == "CYCLES":
        scene.cycles.samples = args.samples
        scene.cycles.use_denoising = False
        scene.cycles.seed = 1
        scene.cycles.device = "CPU"
    else:
        scene.eevee.taa_render_samples = args.samples


def render_views(scene, camera_data, out):
    camera = bpy.data.objects.new("harness", bpy.data.cameras.new("harness"))
    scene.collection.objects.link(camera)
    scene.camera = camera
    center = Vector(camera_data["center"])
    radius = camera_data["radius"]
    camera.data.lens = 50
    camera.data.sensor_width = 36
    camera.data.clip_start = max(radius / 1000, 1e-5)
    camera.data.clip_end = radius * 100
    distance = radius / math.sin(math.atan(18 / 50)) * 1.05
    for name, (azimuth, elevation) in VIEWS.items():
        a, e = math.radians(azimuth), math.radians(elevation)
        direction = Vector((math.cos(e) * math.cos(a), math.cos(e) * math.sin(a), math.sin(e)))
        camera.location = center + direction * distance
        camera.rotation_mode = "QUATERNION"
        camera.rotation_quaternion = (-direction).to_track_quat("-Z", "Y")
        scene.render.filepath = os.path.join(out, f"{name}.png")
        bpy.ops.render.render(write_still=True)


def main():
    args = parse()
    os.makedirs(args.out, exist_ok=True)
    result = {"input": args.input, "ok": False}
    started = time.time()
    try:
        bpy.ops.wm.read_factory_settings(use_empty=True)
        scene = bpy.context.scene
        bpy.ops.import_scene.gltf(filepath=args.input)
        result["import_seconds"] = round(time.time() - started, 2)
        scene = bpy.context.scene
        scene.frame_set(1)
        if args.undo:
            holder = bpy.data.objects.new("undo", None)
            scene.collection.objects.link(holder)
            holder.matrix_world = recovery_matrix(args.input)
            for obj in list(scene.objects):
                if obj is not holder and obj.parent is None:
                    obj.parent = holder
            bpy.context.view_layer.update()
        result["scene"] = measure(scene)
        bounds = result["scene"]["bounds"]
        if args.camera:
            with open(args.camera, encoding="utf-8") as f:
                camera_data = json.load(f)
        else:
            if bounds is None:
                raise RuntimeError("nothing to frame: no mesh in the scene")
            center = [(a + b) / 2 for a, b in zip(*bounds)]
            radius = max(math.dist(bounds[0], bounds[1]) / 2, 1e-4)
            camera_data = {"center": center, "radius": radius}
            with open(os.path.join(args.out, "camera.json"), "w", encoding="utf-8") as f:
                json.dump(camera_data, f)
        world_light(scene)
        configure(scene, args)
        render_views(scene, camera_data, args.out)
        result["ok"] = True
    except Exception:
        result["error"] = traceback.format_exc()[-1500:]
    result["seconds"] = round(time.time() - started, 2)
    with open(os.path.join(args.out, "scene.json"), "w", encoding="utf-8") as f:
        json.dump(result, f, indent=1)


main()
