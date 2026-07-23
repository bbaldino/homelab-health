fn main() {
    // rust-embed's derive requires the `ui/dist/` folder to exist at compile
    // time. In dev/CI the UI may not be built yet, and the folder is gitignored
    // (so a fresh checkout won't have it). Ensure it exists so the crate always
    // compiles — an empty embed just yields the inline fallback page. The Docker
    // build populates `ui/dist` with the real UI before compiling.
    std::fs::create_dir_all("ui/dist").ok();
}
