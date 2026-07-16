fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=嵌入图标失败（不影响编译）: {}", e);
        }
        println!("cargo:rerun-if-changed=assets/icon.ico");
    }
}
