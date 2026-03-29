use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    // djinn/crates/coordin8-proto → repo root is three levels up
    let proto_root = manifest_dir.join("../../../proto");
    let proto_root = proto_root.canonicalize().unwrap_or(proto_root);

    let protos = [
        proto_root.join("coordin8/lease.proto"),
        proto_root.join("coordin8/registry.proto"),
        proto_root.join("coordin8/proxy.proto"),
    ];

    // Tell cargo to re-run if any proto changes
    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        proto_root.join("coordin8/common.proto").display()
    );

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            protos
                .iter()
                .map(|p| p.as_path())
                .collect::<Vec<_>>()
                .as_slice(),
            &[proto_root.as_path()],
        )?;

    Ok(())
}
