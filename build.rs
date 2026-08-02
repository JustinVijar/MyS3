fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: build script is single-threaded; PROTOC is only consumed by tonic-build below.
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    tonic_build::compile_protos("proto/replication.proto")?;
    Ok(())
}
