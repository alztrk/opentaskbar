fn main() {
    println!("cargo:rerun-if-changed=resources/opentaskbar.rc");
    println!("cargo:rerun-if-changed=resources/OpenTaskBar.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resource::compile("resources/opentaskbar.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
