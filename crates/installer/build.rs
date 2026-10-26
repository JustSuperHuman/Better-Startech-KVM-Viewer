use std::{
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

/// Embeds the staged payload (see scripts/package.py). Without
/// CART_INSTALLER_PAYLOAD the setup builds empty and refuses to install.
fn main() {
    println!("cargo:rerun-if-env-changed=CART_INSTALLER_PAYLOAD");
    println!("cargo:rerun-if-changed=setup.rc");
    println!("cargo:rerun-if-changed=setup.manifest");
    println!("cargo:rerun-if-changed=../../assets/icons/cart.ico");
    let mut source = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    if let Some(root) = env::var_os("CART_INSTALLER_PAYLOAD").map(PathBuf::from) {
        let mut files = Vec::new();
        collect(&root, &root, &mut files);
        files.sort();
        assert!(!files.is_empty(), "Installer payload folder is empty");
        for (relative, absolute) in files {
            println!("cargo:rerun-if-changed={}", absolute.display());
            writeln!(
                source,
                "    ({relative:?}, include_bytes!({:?})),",
                absolute.display().to_string()
            )
            .unwrap();
        }
    }
    source.push_str("];\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("payload.rs"), source).unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Only the setup binary requests elevation; test binaries run normally.
        embed_resource::compile_for("setup.rc", ["cart-setup"], embed_resource::NONE)
            .manifest_required()
            .expect("Compile the setup icon, version and elevation resources");
    }
}

fn collect(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let relative = path.strip_prefix(root).unwrap();
            let parts: Vec<_> = relative
                .components()
                .map(|part| part.as_os_str().to_str().unwrap().to_owned())
                .collect();
            files.push((parts.join("/"), path));
        }
    }
}
