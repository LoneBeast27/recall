//! Embeds the Recall icon as resource #1 (GPUI loads it for the window/taskbar), the version
//! resource, and the app manifest: asInvoker plus GPUI's own DPI and common-controls entries
//! (GPUI's `windows-manifest` feature is off so the exe carries exactly one manifest).
include!("../recall/build_winres.rs");

const GPUI_MANIFEST: &str = r#"  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df"/>
    </dependentAssembly>
  </dependency>
"#;

fn main() {
    embed_version_resource("Recall", "recall-native.exe", Some("assets/recall.ico"), GPUI_MANIFEST);
}