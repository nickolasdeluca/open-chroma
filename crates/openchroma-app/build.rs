fn main() {
    // fluent-dark styles the few stock widgets used (combo boxes, scroll bars)
    // to sit on the app's dark theme.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compile UI");
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().expect("embed icon");
}
