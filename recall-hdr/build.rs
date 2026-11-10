// Embeds the version resource and asInvoker manifest (../recall/build_winres.rs).
include!("../recall/build_winres.rs");

fn main() {
    embed_version_resource("Recall controller", "recall-hdr.exe", None, "");
}