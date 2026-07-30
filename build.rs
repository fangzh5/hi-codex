fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/hicodex_icon/hicodex.ico");
        resource.set("ProductName", "HiCodex");
        resource.set("FileDescription", "HiCodex");
        resource.set("OriginalFilename", "HiCodex.exe");
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            let output_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
            let resource_file = output_dir.join("resource.rc");
            let resource_object = output_dir.join("resource.o");
            resource
                .write_resource_file(&resource_file)
                .expect("could not write Windows icon resources");
            let windres = std::env::var_os("WINDRES").unwrap_or_else(|| "windres".into());
            let status = std::process::Command::new(windres)
                .arg("--target=pe-x86-64")
                .arg(format!(
                    "-I{}",
                    std::env::var("CARGO_MANIFEST_DIR").unwrap()
                ))
                .arg(&resource_file)
                .arg(&resource_object)
                .status()
                .expect("could not run windres");
            assert!(status.success(), "could not compile Windows icon resources");
            println!("cargo:rustc-link-arg-bins={}", resource_object.display());
        } else {
            resource
                .compile()
                .expect("could not compile Windows icon resources");
        }
    }
}
