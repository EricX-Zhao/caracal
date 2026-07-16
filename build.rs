fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/caracal.ico");
        res.compile().expect("failed to embed Windows icon resource");
    }
}
