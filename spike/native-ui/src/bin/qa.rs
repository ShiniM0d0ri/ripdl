//! CDP-like QA client for the native spike.
//!
//! Connects to eframe's `EGUI_INSPECTION` TCP port, reads the AccessKit tree,
//! captures a screenshot, and prints a compact tree summary. This is the
//! native equivalent of the CDP DOM/screenshot checks used for the Tauri UI.

use std::net::TcpStream;
use std::time::Duration;

use egui_inspection::protocol::{read_handshake, read_message, write_message, Request, Response};

fn main() -> anyhow::Result<()> {
    let addr =
        std::env::var("EGUI_INSPECTION_ADDR").unwrap_or_else(|_| "127.0.0.1:5719".to_string());
    let label = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "screen".to_string());

    let mut stream = TcpStream::connect(&addr)
        .map_err(|e| anyhow::anyhow!("inspection port {addr} not open: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let version = read_handshake(&mut stream)?;
    println!("egui inspection protocol v{version}");

    write_message(&mut stream, &Request::GetInfo)?;
    match read_message::<_, Response>(&mut stream)? {
        Response::Info {
            label,
            egui_version,
        } => {
            println!("app={:?} egui={egui_version}", label.unwrap_or_default());
        }
        other => println!("info: {other:?}"),
    }

    write_message(&mut stream, &Request::GetTree)?;
    match read_message::<_, Response>(&mut stream)? {
        Response::Tree {
            step,
            pixels_per_point,
            accesskit,
        } => {
            let mut names = Vec::new();
            if let Some(tree) = accesskit {
                for (_id, node) in tree.nodes {
                    names.push(format!("{:?}", node.role()));
                }
            }
            println!(
                "tree step={step} ppp={pixels_per_point} named_nodes={}",
                names.len()
            );
            for n in names.iter().take(40) {
                println!("  {n}");
            }
        }
        other => println!("tree: {other:?}"),
    }

    write_message(
        &mut stream,
        &Request::GetScreenshot {
            pixels_per_point: Some(1.0),
        },
    )?;
    match read_message::<_, Response>(&mut stream)? {
        Response::Screenshot(png) => {
            let shots = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qa/shots");
            std::fs::create_dir_all(&shots)?;
            let path = shots.join(format!("{label}.png"));
            std::fs::write(&path, &png.bytes)?;
            println!(
                "screenshot {}x{} -> {}",
                png.size[0],
                png.size[1],
                path.display()
            );
        }
        other => println!("screenshot: {other:?}"),
    }
    Ok(())
}
