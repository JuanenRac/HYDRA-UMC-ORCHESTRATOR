// HYDRA-UMC-ORCHESTRATOR - build.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// Generates the gRPC bindings of the shared contract (proto/hydra_common.proto) into the
// build directory; nothing generated is committed. protoc is the one packaged in
// protoc-bin-vendored, so the build needs no tool installed on the machine.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/hydra_common.proto");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("PROTOC").is_none() {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    tonic_prost_build::configure().compile_protos(&["proto/hydra_common.proto"], &["proto"])?;
    Ok(())
}
