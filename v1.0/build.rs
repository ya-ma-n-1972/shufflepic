//! ビルドスクリプト：Windows 実行ファイルにアプリアイコン（exe のファイルアイコン）を埋め込む。
//! アイコン素材はリポジトリ共有の `../assets/app.ico`（16/32/48/256 内包）。

fn main() {
    println!("cargo:rerun-if-changed=../assets/app.ico");

    #[cfg(windows)]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let icon = std::path::Path::new(&manifest_dir).join("../assets/app.ico");
        // 解決できれば絶対パスで、できなければそのまま渡す。
        let icon_path = std::fs::canonicalize(&icon).unwrap_or(icon);
        if let Some(p) = icon_path.to_str() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon(p);
            // SDK の rc.exe が無い環境ではアイコン無しでビルド継続（致命的にしない）。
            if let Err(e) = res.compile() {
                println!("cargo:warning=Windows アイコンの埋め込みに失敗しました: {e}");
            }
        }
    }
}
