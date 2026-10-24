fn main() {
    // rust-i18n reads these through a proc macro; Cargo must also track them
    // so an incremental build picks up changes to menu/help translations.
    println!("cargo:rerun-if-changed=locales");
}
