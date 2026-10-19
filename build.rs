//! Build script: generates the application icon and, when building on a
//! Windows host for a Windows target, embeds the icon, an application
//! manifest (per-monitor-v2 DPI awareness, Common Controls v6, asInvoker)
//! and version information into the executable.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
mod icon_art {
    include!("src/tray/icon_art.rs");
}

#[cfg(windows)]
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="SwiftVoice" version="@VERSION@" processorArchitecture="*"/>
  <description>SwiftVoice offline voice typing</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/tray/icon_art.rs");

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let ico = out.join("swiftvoice.ico");
    let bytes = icon_art::encode_ico(&[16, 20, 24, 32, 40, 48, 64, 256], icon_art::IconTone::Idle);
    fs::write(&ico, bytes).expect("failed to write generated icon");
    embed_resources(&ico);
}

#[cfg(windows)]
fn embed_resources(ico: &Path) {
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    let version = format!("{}.{}.{}.0", env!("CARGO_PKG_VERSION_MAJOR"), env!("CARGO_PKG_VERSION_MINOR"), env!("CARGO_PKG_VERSION_PATCH"));
    res.set_manifest(&MANIFEST.replace("@VERSION@", &version));
    res.set("FileDescription", "SwiftVoice offline voice typing");
    res.set("ProductName", "SwiftVoice");
    res.set("CompanyName", "SwiftVoice contributors");
    res.set("LegalCopyright", "MIT License");
    res.set("OriginalFilename", "swiftvoice.exe");
    if let Err(e) = res.compile() {
        // Not fatal: DPI awareness is also set at run time; only the
        // Explorer icon and version tab would be missing.
        println!(
            "cargo:warning=SwiftVoice: could not embed icon/manifest resources ({e}). \
             Install the Windows SDK (rc.exe) to embed them."
        );
    }
}

#[cfg(not(windows))]
fn embed_resources(_ico: &Path) {
    println!(
        "cargo:warning=SwiftVoice: cross-building for Windows from a non-Windows host; \
         the icon and manifest resources are only embedded when building on Windows."
    );
}
