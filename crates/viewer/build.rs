fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=../../assets/icons/cart.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Compile the viewer's Windows icon and version resources");
    }
}
