//! The `voxview` binary: argument parsing, `--stats`, and the hand-off to
//! [`voxview::app::run`].
//!
//! Everything else lives in the library half of the crate, which is where the
//! documentation is.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use voxview::loader;
use voxview::settings::{self, Settings};

/// A viewer and browser for MagicaVoxel `.vox` files.
#[derive(Parser, Debug)]
#[command(name = "voxview", version, about, long_about = None)]
struct Args {
    /// A `.vox` file to view, or a directory to browse.
    path: Option<PathBuf>,

    /// Print a summary of the file and exit without opening a window.
    #[arg(long)]
    stats: bool,

    /// Write the test fixtures into a directory and exit.
    #[arg(long, value_name = "DIR")]
    write_fixtures: Option<PathBuf>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("voxview: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    if let Some(dir) = &args.write_fixtures {
        voxview::fixtures::write_all(dir)?;
        println!("wrote fixtures to {}", dir.display());
        return Ok(());
    }

    // `--stats` is a command-line tool and has nothing to remember, so it
    // never touches the settings file.
    if args.stats {
        let path = args.path.as_deref().context("--stats needs a path")?;
        let (paths, index) = loader::collect_vox_paths(path)?;
        let scene = loader::load_file(&paths[index])?;
        print_stats(&paths[index], &scene);
        return Ok(());
    }

    let settings = Settings::load();
    // With no argument, reopen the directory the library was last pointed at.
    // That is what every other browser does, and it turns voxview into
    // something you can pin to a taskbar.
    let path = match args.path.as_deref() {
        Some(path) => path.to_path_buf(),
        None => settings
            .root
            .clone()
            .filter(|root| settings::usable_root(root))
            .ok_or_else(|| anyhow::anyhow!("a .vox file or directory is required"))?,
    };
    let (paths, index) = loader::collect_vox_paths(&path)?;
    // A directory argument means "show me what is here", so it opens the
    // library; naming one file means "show me this", so it opens the viewer.
    let browse = path.is_dir();
    voxview::app::run(loader::browse_root(&path), paths, index, browse, settings)
}

fn print_stats(path: &std::path::Path, scene: &loader::VoxScene) {
    let dims = scene.dimensions();
    println!("{}", path.display());
    println!("  version       {}", scene.version);
    println!("  models        {}", scene.models.len());
    println!("  instances     {}", scene.instances.len());
    println!("  voxels        {}", scene.voxel_count);
    println!("  dimensions    {} x {} x {}", dims.x, dims.y, dims.z);
    println!(
        "  bounds        min {:?} max {:?}",
        scene.bounds.min.to_array(),
        scene.bounds.max.to_array()
    );
    println!(
        "  palette       {}",
        if scene.palette.from_file {
            "from file"
        } else {
            "MagicaVoxel default"
        }
    );
    println!(
        "  materials     {}{}",
        scene.material_count,
        describe_materials(&scene.materials)
    );
    for (i, m) in scene.models.iter().enumerate() {
        let s = m.size();
        println!(
            "  model {i:<3}     {} x {} x {}  ({} voxels)",
            s.x,
            s.y,
            s.z,
            m.voxel_count()
        );
    }
}

/// The part of a material table worth putting in one line: not how many
/// `MATL` chunks a file has -- MagicaVoxel writes 256 whether they say
/// anything or not -- but how many of them this renderer will act on.
fn describe_materials(materials: &voxview::material::Materials) -> String {
    let parts = [
        (materials.emissive_count(), "emissive"),
        (materials.metal_count(), "metal"),
        (materials.transparent_count(), "transparent"),
    ];
    let listed: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, name)| format!("{n} {name}"))
        .collect();
    if listed.is_empty() {
        String::new()
    } else {
        format!("  ({})", listed.join(", "))
    }
}
