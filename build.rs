fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        let ico = r"G:\Strata-3060\icons\am2strata.ico";
        if std::path::Path::new(ico).exists() {
            res.set_icon(ico);
        }
        res.set("ProductName", "am2Strata Desktop");
        res.set("FileDescription", "am2Strata Local AI Desktop Workspace");
        res.compile().expect("failed to compile Windows resources");
    }
}
