// Встраивает иконку приложения в .exe при сборке под Windows.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        // Не валим сборку, если rc-тулчейн недоступен — иконку окна (= панель
        // задач) всё равно ставим в рантайме (main.rs set_window_icon).
        if let Err(e) = res.compile() {
            println!("cargo:warning=exe icon embed skipped: {e}");
        }
    }
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
}
