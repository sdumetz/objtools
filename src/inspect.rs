use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use indexmap::IndexSet;
use serde::Serialize;

use crate::fixed::{parse_three_fixed, BBox, BBoxRecord};
use crate::format::{fmt_bytes, fmt_verts};

#[derive(Serialize)]
struct ObjectRecord {
    name: String,
    vertex_count: u64,
    materials: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bounding_box: Option<BBoxRecord>,
}

#[derive(Serialize)]
struct Summary {
    total_objects: usize,
    total_vertices: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bounding_box: Option<BBoxRecord>,
    objects: Vec<ObjectRecord>,
}

fn print_human(summary: &Summary, out: &mut impl Write) -> std::io::Result<()> {
    let n = summary.total_objects;
    writeln!(out, "┌─ {} object{}", n, if n == 1 { "" } else { "s" })?;

    for (i, obj) in summary.objects.iter().enumerate() {
        let is_last = i == n - 1;
        let branch = if is_last { "└──" } else { "├──" };
        let indent = if is_last { "    " } else { "│   " };

        writeln!(out, "{} 「{}」", branch, obj.name)?;
        writeln!(
            out,
            "{}  ├─ {} {}",
            indent,
            fmt_verts(obj.vertex_count),
            if obj.vertex_count == 1 { "vertex" } else { "vertices" }
        )?;

        if obj.materials.is_empty() {
            writeln!(out, "{}  ├─ no materials", indent)?;
        } else {
            writeln!(
                out,
                "{}  ├─ {} material{}: {}",
                indent,
                obj.materials.len(),
                if obj.materials.len() == 1 { "" } else { "s" },
                obj.materials.join(", ")
            )?;
        }

        match &obj.bounding_box {
            Some(b) => writeln!(
                out,
                "{}  └─ bounds {} → {}",
                indent,
                b.min.join(" "),
                b.max.join(" ")
            )?,
            None => writeln!(out, "{}  └─ no bounds", indent)?,
        }
    }

    writeln!(out)?;
    writeln!(
        out,
        "   total   {} vertices in {} object{}",
        fmt_verts(summary.total_vertices),
        n,
        if n == 1 { "" } else { "s" }
    )?;
    if let Some(b) = &summary.bounding_box {
        writeln!(out, "   bounds  {} → {}", b.min.join(" "), b.max.join(" "))?;
        writeln!(out, "   size    {} × {} × {}", b.size[0], b.size[1], b.size[2])?;
        writeln!(out, "   center  {}", b.center.join(" "))?;
    }

    Ok(())
}

pub struct InspectOptions {
    pub file_path: String,
    pub json: bool,
    pub compact: bool,
    pub progress: bool,
}

pub fn run(opts: InspectOptions) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(&opts.file_path)
        .map_err(|e| format!("{}: {}", opts.file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, file);

    let mut objects: Vec<ObjectRecord> = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_verts: u64 = 0;
    let mut current_mats: IndexSet<String> = IndexSet::new();
    let mut current_bbox = BBox::new();
    let mut total_bbox = BBox::new();
    let mut total_verts: u64 = 0;
    let mut bytes_read: u64 = 0;
    let mut next_progress: u64 = 100_000_000;

    let flush = |objects: &mut Vec<ObjectRecord>,
                 name: &mut Option<String>,
                 verts: &mut u64,
                 mats: &mut IndexSet<String>,
                 bbox: &mut BBox,
                 total: &mut BBox| {
        if let Some(n) = name.take() {
            objects.push(ObjectRecord {
                name: n,
                vertex_count: *verts,
                materials: mats.drain(..).collect(),
                bounding_box: bbox.to_record(),
            });
        }
        total.merge(bbox);
        *verts = 0;
        *bbox = BBox::new();
    };

    for line in reader.lines() {
        let line = line?;
        bytes_read += line.len() as u64 + 1;

        if opts.progress && bytes_read >= next_progress {
            next_progress = bytes_read + 100_000_000;
            let obj_count = objects.len() + 1;
            eprintln!(
                "Read {}, {} object{} so far",
                fmt_bytes(bytes_read),
                obj_count,
                if obj_count == 1 { "" } else { "s" }
            );
        }

        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        match line.split_once(' ') {
            Some(("o", rest)) => {
                flush(
                    &mut objects,
                    &mut current_name,
                    &mut current_verts,
                    &mut current_mats,
                    &mut current_bbox,
                    &mut total_bbox,
                );
                current_name = Some(rest.trim().to_string());
            }
            Some(("v", rest)) => {
                current_verts += 1;
                total_verts += 1;
                // A coordinate we cannot parse is still a vertex; it just does not move the box.
                if let Ok(coords) = parse_three_fixed(rest) {
                    current_bbox.add(coords);
                }
            }
            Some(("usemtl", rest)) => {
                current_mats.insert(rest.trim().to_string());
            }
            _ => {}
        }
    }

    if current_name.is_some() {
        flush(
            &mut objects,
            &mut current_name,
            &mut current_verts,
            &mut current_mats,
            &mut current_bbox,
            &mut total_bbox,
        );
    } else if current_verts > 0 || !current_mats.is_empty() {
        objects.push(ObjectRecord {
            name: "(default)".to_string(),
            vertex_count: current_verts,
            materials: current_mats.drain(..).collect(),
            bounding_box: current_bbox.to_record(),
        });
        total_bbox.merge(&current_bbox);
    }

    let summary = Summary {
        total_objects: objects.len(),
        total_vertices: total_verts,
        bounding_box: total_bbox.to_record(),
        objects,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if opts.json {
        if opts.compact {
            serde_json::to_writer(&mut out, &summary)?;
            writeln!(out)?;
        } else {
            serde_json::to_writer_pretty(&mut out, &summary)?;
            writeln!(out)?;
        }
    } else {
        print_human(&summary, &mut out)?;
    }

    Ok(())
}
