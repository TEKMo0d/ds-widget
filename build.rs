fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();

        res.set_icon("assets/icon.ico");

        let version = env!("CARGO_PKG_VERSION");

        res.set("ProductName", "DeepSeek Widget");
        res.set("FileDescription", "DeepSeek Widget by kakenhi");
        res.set("CompanyName", "kakenhi");
        res.set("ProductVersion", version);
        res.set("FileVersion", version);

        if let Err(e) = res.compile() {
            println!("cargo:warning=嵌入 Windows 资源失败（不影响编译）: {}", e);
        }

        println!("cargo:rerun-if-changed=assets/icon.ico");
    }
}