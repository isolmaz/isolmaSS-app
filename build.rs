use std::path::PathBuf;
use std::process::Command;

fn parse_version(name: &str) -> Option<Vec<u64>> {
    if name.is_empty() {
        return None;
    }
    name.split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse::<u64>().ok()
        })
        .collect()
}

fn resource_compiler() -> PathBuf {
    if let Some(configured) = std::env::var_os("RC") {
        return PathBuf::from(configured);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(sdk) = std::env::var_os("WindowsSdkDir") {
        roots.push(PathBuf::from(sdk).join("bin"));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(var) {
            roots.push(
                PathBuf::from(base)
                    .join("Windows Kits")
                    .join("10")
                    .join("bin"),
            );
        }
    }
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    let mut flat: Option<PathBuf> = None;
    for root in &roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let Some(version) = entry.file_name().to_str().and_then(parse_version) else {
                    continue;
                };
                let candidate = entry.path().join("x64").join("rc.exe");
                if candidate.is_file()
                    && best
                        .as_ref()
                        .is_none_or(|(best_version, _)| version > *best_version)
                {
                    best = Some((version, candidate));
                }
            }
        }
        let candidate = root.join("x64").join("rc.exe");
        if candidate.is_file() && flat.is_none() {
            flat = Some(candidate);
        }
    }
    if let Some((_, candidate)) = best {
        return candidate;
    }
    if let Some(candidate) = flat {
        return candidate;
    }
    // No SDK installation found. rc.exe on PATH is a legitimate configuration,
    // so try PATH and let the launch error explain what was searched.
    PathBuf::from("rc.exe")
}

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let resources = manifest_dir.join("resources");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"))
        .join("app.res");
    if let Ok(pins) = std::env::var("ISOLMASS_UPDATE_PUBLIC_KEYS") {
        assert!(
            pins.is_empty()
                || pins.split(';').count() <= 8
                    && pins
                        .split(';')
                        .all(|pin| pin.len() == 64
                            && pin.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "ISOLMASS_UPDATE_PUBLIC_KEYS must contain at most eight semicolon-separated SHA-256 public-key digests"
        );
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let numeric = format!("{},0", version.replace('.', ","));
    let generated = output.with_extension("rc");
    let manifest = output.with_extension("manifest");
    let manifest_template =
        std::fs::read_to_string(resources.join("app.manifest")).expect("read manifest template");
    std::fs::write(
        &manifest,
        manifest_template.replace("@PRODUCT_VERSION@", &format!("{version}.0")),
    )
    .expect("write versioned manifest");
    let version_info = format!(
        r#"
#include "{}"
1 24 "{}"
1 VERSIONINFO
FILEVERSION {}
PRODUCTVERSION {}
FILEFLAGSMASK 0x3fL
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "isolmaSS contributors\0"
      VALUE "FileDescription", "isolmaSS screenshot utility\0"
      VALUE "FileVersion", "{}\0"
      VALUE "InternalName", "isolmass\0"
      VALUE "LegalCopyright", "Copyright (c) 2026 isolmaSS contributors. MIT License.\0"
      VALUE "OriginalFilename", "isolmass.exe\0"
      VALUE "ProductName", "isolmaSS\0"
      VALUE "ProductVersion", "{}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#,
        resources
            .join("app.rc")
            .display()
            .to_string()
            .replace('\\', "/"),
        manifest.display().to_string().replace('\\', "/"),
        numeric,
        numeric,
        version,
        version
    );
    std::fs::write(&generated, version_info).expect("write Windows version resources");
    let status = Command::new(resource_compiler())
        .current_dir(&resources)
        .args(["/nologo", "/fo"])
        .arg(&output)
        .arg(&generated)
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => panic!(
            "failed to launch the Windows resource compiler ({error}). \
             Set RC to the full path of rc.exe, install the Windows 10/11 SDK \
             (rc.exe is searched in %WindowsSdkDir%bin and \
             %ProgramFiles(x86)%\\Windows Kits\\10\\bin under <version>\\x64), \
             or put rc.exe on PATH"
        ),
    };
    assert!(status.success(), "Windows resource compilation failed");

    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WindowsSdkDir");
    println!("cargo:rerun-if-env-changed=ISOLMASS_UPDATE_PUBLIC_KEYS");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.ico");
    println!("cargo:rerun-if-changed=resources/app.manifest");
    println!("cargo:rustc-link-arg={}", output.display());
}
