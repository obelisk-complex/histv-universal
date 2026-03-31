fn main() {
    #[cfg(feature = "custom-protocol")]
    tauri_build::build();
}
