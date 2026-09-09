use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use indexmap::IndexSet;
use serde::Serialize;

use crate::format::{fmt_bytes, fmt_verts};

#[derive(Serialize)]
struct ObjectRecord {
    name: String,
    vertex_count: u64,
    materials: Vec<String>,
}

#[derive(Serialize)]
struct Summary {
    total_objects: usize,
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
            writeln!(out, "{}  └─ no materials", indent)?;
        } else {
            writeln!(
                out,
                "{}  └─ {} material{}: {}",
                indent,
                obj.materials.len(),
                if obj.materials.len() == 1 { "" } else { "s" },
                obj.materials.join(", ")
            )?;
        }
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
    let mut bytes_read: u64 = 0;
    let mut next_progress: u64 = 100_000_000;

    let flush = |objects: &mut Vec<ObjectRecord>,
                 name: &mut Option<String>,
                 verts: &mut u64,
                 mats: &mut IndexSet<String>| {
        if let Some(n) = name.take() {
            objects.push(ObjectRecord {
                name: n,
                vertex_count: *verts,
                materials: mats.drain(..).collect(),
            });
        }
        *verts = 0;
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
                flush(&mut objects, &mut current_name, &mut current_verts, &mut current_mats);
                current_name = Some(rest.trim().to_string());
            }
            Some(("v", _)) => {
                current_verts += 1;
            }
            Some(("usemtl", rest)) => {
                current_mats.insert(rest.trim().to_string());
            }
            _ => {}
        }
    }

    if current_name.is_some() {
        flush(&mut objects, &mut current_name, &mut current_verts, &mut current_mats);
    } else if current_verts > 0 || !current_mats.is_empty() {
        objects.push(ObjectRecord {
            name: "(default)".to_string(),
            vertex_count: current_verts,
            materials: current_mats.drain(..).collect(),
        });
    }

    let summary = Summary {
        total_objects: objects.len(),
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
