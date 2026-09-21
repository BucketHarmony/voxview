use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use voxview::loader;

/// A viewer for MagicaVoxel `.vox` files.
#[derive(Parser, Debug)]
#[command(name = "voxview", version, about, long_about = None)]
struct Args {
    /// A `.vox` file, or a directory to page through with `[` and `]`.
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

    let path = args
        .path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("a .vox file or directory is required"))?;
    let (paths, index) = loader::collect_vox_paths(path)?;
    if args.stats {
        let scene = loader::load_file(&paths[index])?;
        print_stats(&paths[index], &scene);
        return Ok(());
    }
    voxview::app::run(paths, index)
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
        "  materials     {} (read, not rendered)",
        scene.material_count
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
