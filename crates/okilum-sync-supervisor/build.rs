use std::{env, path::PathBuf};

/// Windows: the same version (and icon, when the build provides one) as the
/// app, so the helper is not a bare file name in Task Manager (#1037).
fn main() {
    println!("cargo:rerun-if-env-changed=OKILUM_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=OKILUM_WINDOWS_ICON");
    println!("cargo:rerun-if-changed=resources/windows/okilum-sync.rc");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version =
        env::var("OKILUM_RELEASE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
    let icon = env::var("OKILUM_WINDOWS_ICON")
        .map(|icon| {
            println!("cargo:rerun-if-changed={icon}");
            format!("1 ICON \"{}\"", icon.replace('\\', "/"))
        })
        .unwrap_or_default();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource = out.join("okilum-sync.rc");
    let template = std::fs::read_to_string("resources/windows/okilum-sync.rc").unwrap();
    std::fs::write(
        &resource,
        template
            .replace("@ICON_LINE@", &icon)
            .replace("@VERSION@", &version),
    )
    .unwrap();
    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_optional()
        .expect("Windows resources must compile");
}
