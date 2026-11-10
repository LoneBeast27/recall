// Shared by the build scripts of every shipped Recall exe (`include!`d, so no extra crate):
// a VERSIONINFO block and an asInvoker manifest, compiled with embed-resource.
//
// The version is RECALL_VERSION when set (scripts/release.ps1 sets it), else the crate version.
// Every shipped crate carries the same crate version, so a dev build of each exe agrees too.

/// Writes `<OUT_DIR>/recall-version.rc` plus its manifest and links them into this crate's bins.
/// `manifest_extra` is spliced inside `<assembly>` (recall-native adds GPUI's DPI and
/// common-controls entries there); `icon` is a path relative to the crate root.
#[allow(dead_code)]
fn embed_version_resource(description: &str, file_name: &str, icon: Option<&str>, manifest_extra: &str) {
    use std::{env, fs, path::PathBuf};
    println!("cargo:rerun-if-env-changed=RECALL_VERSION");
    println!("cargo:rerun-if-changed=../recall/build_winres.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = match env::var("RECALL_VERSION") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_owned(),
        _ => env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"),
    };
    let numbers: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or_else(|_| panic!("version {version:?} is not a.b.c"))).collect();
    assert!(numbers.len() == 3, "version {version:?} is not a.b.c");
    let binary = format!("{},{},{},0", numbers[0], numbers[1], numbers[2]);
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = out.join("recall.manifest");
    fs::write(&manifest, format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
{manifest_extra}</assembly>
"#)).expect("write manifest");
    let rc_path = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
    let icon = icon.map(|i| {
        let path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join(i);
        println!("cargo:rerun-if-changed={i}");
        format!("1 ICON \"{}\"\n", rc_path(&path))
    }).unwrap_or_default();
    let internal = file_name.trim_end_matches(".exe");
    let rc = format!(r#"#pragma code_page(65001)
{icon}1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {binary}
PRODUCTVERSION {binary}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "LoneBeast27"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{internal}"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "{file_name}"
      VALUE "ProductName", "Recall"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#, manifest = rc_path(&manifest));
    let rc_file = out.join("recall-version.rc");
    fs::write(&rc_file, rc).expect("write rc");
    embed_resource::compile(&rc_file, embed_resource::NONE).manifest_required().expect("compile the version resource");
}
