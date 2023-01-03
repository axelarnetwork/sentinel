use std::io::Result;
fn main() -> Result<()> {
    prost_build::compile_protos(&["proto/grpc.proto", "proto/multisig.proto"], &["proto/"])?;
    Ok(())
}
