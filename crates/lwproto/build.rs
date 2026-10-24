fn main() {
    println!("cargo:rerun-if-changed=proto");
    // Message types plus CompactTxStreamer client/server stubs. The
    // pass-through proxy forwards raw frames and only uses the messages;
    // the verifying gateway serves the trait and calls the indexer with
    // the client.
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &["proto/service.proto", "proto/ssg/record.proto"],
            &["proto"],
        )
        .expect("compiling protocol definitions");
}
