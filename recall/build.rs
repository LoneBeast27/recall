// Export the hybrid-GPU opt-in symbols (src/hybrid_gpu.rs) from the engine binary only,
// and embed the version resource and asInvoker manifest (build_winres.rs).
include!("build_winres.rs");

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for symbol in ["NvOptimusEnablement", "AmdPowerXpressRequestHighPerformance"] {
            println!("cargo:rustc-link-arg-bin=recall=/EXPORT:{symbol}");
        }
    }
    embed_version_resource("Recall recorder engine", "recall.exe", None, "");
}