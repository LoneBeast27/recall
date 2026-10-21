//! Embeds the Recall icon as resource #1, which GPUI loads for the window/taskbar.
fn main() {
    println!("cargo:rerun-if-changed=recall.rc");
    println!("cargo:rerun-if-changed=assets/recall.ico");
    embed_resource::compile("recall.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("embed Recall icon resource");
}
