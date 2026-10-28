fn main() {
    // Tauri embeds its default manifest only in binaries. Native UI unit tests
    // also need Common Controls v6 (TaskDialogIndirect is absent from v5).
    // Embed that manifest once in every target; keep Tauri's icon/version resources.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("failed to build Tauri resources");
    println!("cargo:rerun-if-changed=windows/common-controls.rc");
    println!("cargo:rerun-if-changed=windows/common-controls.manifest");
    embed_resource::compile_for_everything("windows/common-controls.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed Common Controls manifest");
}
