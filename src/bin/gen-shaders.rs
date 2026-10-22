//! Regenerate the committed WGSL shader files under `shaders/`.
//!
//! ```text
//! cargo run --bin gen-shaders            # writes ./shaders
//! cargo run --bin gen-shaders -- DIR     # writes DIR
//! ```
//!
//! The official `emotedriver.dll` contains 58 unique D3D9 programs
//! (27 ps_2_0, 27 ps_3_0, 3 vs_2_0, 1 vs_3_0). This tool emits one WGSL file
//! per variant using the feature table in `yu_emote::render`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use yu_emote::render::{
    PIXEL_PROGRAMS, ShaderModel, VertexProgram, generated_pixel_wgsl, generated_vertex_wgsl,
};

fn main() -> ExitCode {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("shaders"));
    match write_all(&root) {
        Ok(count) => {
            println!("wrote {count} shader files under {}", root.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}: {error}", root.display());
            ExitCode::FAILURE
        }
    }
}

fn write_all(root: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    for model in [ShaderModel::Ps2, ShaderModel::Ps3] {
        let directory = root.join(model.directory());
        fs::create_dir_all(&directory)?;
        for program in &PIXEL_PROGRAMS {
            let path = directory.join(format!("{:02}.wgsl", program.index));
            fs::write(&path, generated_pixel_wgsl(model, program))?;
            count += 1;
        }
    }

    let vs2 = root.join("vs_2_0");
    fs::create_dir_all(&vs2)?;
    fs::write(
        vs2.join("00.wgsl"),
        generated_vertex_wgsl("vs_2_0", VertexProgram::Basic),
    )?;
    fs::write(
        vs2.join("01.wgsl"),
        generated_vertex_wgsl("vs_2_0", VertexProgram::ScreenOffset),
    )?;
    fs::write(
        vs2.join("02.wgsl"),
        generated_vertex_wgsl("vs_2_0", VertexProgram::ScreenOffsetScaled),
    )?;
    count += 3;

    let vs3 = root.join("vs_3_0");
    fs::create_dir_all(&vs3)?;
    fs::write(
        vs3.join("00.wgsl"),
        generated_vertex_wgsl("vs_3_0", VertexProgram::Basic),
    )?;
    count += 1;

    Ok(count)
}
